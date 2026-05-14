// SPDX-License-Identifier: MIT OR Apache-2.0

use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::ExitCode;

#[cfg(target_os = "hermit")]
use hermit as _;

use serde_json::json;
use waymark_runtime::{pipeline_input_from_bytes, FrontendKind, StoneGuest};
use waymark_runtime_support::{configure_process_environment, default_start_dir};

fn main() -> ExitCode {
    let start_dir = default_start_dir();
    configure_process_environment(&start_dir);

    let mut guest = match StoneGuest::new(start_dir) {
        Ok(guest) => guest,
        Err(err) => {
            emit_json_error("failed to initialize guest shell", err.to_string());
            return ExitCode::from(1);
        }
    };

    let program_name = std::env::args()
        .next()
        .and_then(|path| {
            Path::new(&path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "waymark".to_owned());
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    let eval_mode = args.first().is_some_and(|arg| arg == "eval");
    if eval_mode {
        args.remove(0);
    }
    if args
        .first()
        .is_some_and(|arg| arg == "help" || arg == "--help" || arg == "-h")
    {
        println!("{}", usage(&program_name));
        return ExitCode::SUCCESS;
    }
    strip_trailing_hermit_boot_args(&mut args);
    let read_stdin = remove_flag(&mut args, "--stdin");
    let read_stdin_script = remove_flag(&mut args, "--stdin-script");
    let task_server = remove_flag(&mut args, "--task-server");
    let task_server_stream = remove_flag(&mut args, "--task-server-stream");
    let task_server_vsock =
        remove_optional_value(&mut args, "--task-server-vsock", "9975").map(|value| {
            value
                .parse::<u32>()
                .map_err(|err| format!("invalid --task-server-vsock port `{value}`: {err}"))
        });
    let stone = remove_flag(&mut args, "--stone");
    let nu = remove_flag(&mut args, "--nu");
    let task_path = remove_optional_value(&mut args, "--task", "/work/task/task.json");

    let task_server_vsock = match task_server_vsock {
        Some(Ok(port)) => Some(port),
        Some(Err(err)) => {
            emit_json_error("usage error", err);
            return ExitCode::from(2);
        }
        None => None,
    };

    if stone && nu {
        emit_json_error("usage error", "--stone and --nu are mutually exclusive");
        return ExitCode::from(2);
    }

    if task_server && (task_server_stream || task_server_vsock.is_some()) {
        emit_json_error(
            "usage error",
            "--task-server, --task-server-stream, and --task-server-vsock are mutually exclusive",
        );
        return ExitCode::from(2);
    }

    if task_server_stream && task_server_vsock.is_some() {
        emit_json_error(
            "usage error",
            "--task-server-stream and --task-server-vsock are mutually exclusive",
        );
        return ExitCode::from(2);
    }

    if task_server
        && (task_path.is_some()
            || read_stdin
            || read_stdin_script
            || stone
            || nu
            || !args.is_empty())
    {
        emit_json_error(
            "usage error",
            "--task-server cannot be combined with other execution modes",
        );
        return ExitCode::from(2);
    }

    if task_server_stream
        && (task_path.is_some()
            || read_stdin
            || read_stdin_script
            || stone
            || nu
            || !args.is_empty())
    {
        emit_json_error(
            "usage error",
            "--task-server-stream cannot be combined with other execution modes",
        );
        return ExitCode::from(2);
    }

    if task_server_vsock.is_some()
        && (task_path.is_some()
            || read_stdin
            || read_stdin_script
            || stone
            || nu
            || !args.is_empty())
    {
        emit_json_error(
            "usage error",
            "--task-server-vsock cannot be combined with other execution modes",
        );
        return ExitCode::from(2);
    }

    if task_path.is_some() && (read_stdin || read_stdin_script || stone || nu || !args.is_empty()) {
        emit_json_error(
            "usage error",
            "--task cannot be combined with other execution modes",
        );
        return ExitCode::from(2);
    }

    if task_server {
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut reader = stdin.lock();
        let mut writer = stdout.lock();

        if let Err(err) = waymark_runtime::run_task_server(&mut guest, &mut reader, &mut writer) {
            emit_json_error("task server error", err.to_string());
            return ExitCode::from(1);
        }

        return ExitCode::SUCCESS;
    }

    if task_server_stream {
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut reader = stdin.lock();
        let mut writer = stdout.lock();
        let mut stream = StdioTaskStream {
            reader: &mut reader,
            writer: &mut writer,
        };

        if let Err(err) = waymark_runtime::run_task_server_stream(&mut guest, &mut stream) {
            emit_json_error("task server stream error", err.to_string());
            return ExitCode::from(1);
        }

        return ExitCode::SUCCESS;
    }

    if let Some(port) = task_server_vsock {
        if let Err(err) = waymark_runtime::run_vsock_task_server(&mut guest, port) {
            emit_json_error("vsock task server error", err.to_string());
            return ExitCode::from(1);
        }

        return ExitCode::SUCCESS;
    }

    if let Some(path) = task_path {
        let response = if path == "/work/task/task.json" {
            guest.task_response_from_default_path()
        } else {
            guest.task_response_from_path(&path)
        };
        let ok = response
            .get("ok")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        emit_json_response(response, ok);
        return ExitCode::from(if ok { 0 } else { 1 });
    }

    if read_stdin && read_stdin_script {
        emit_json_error(
            "usage error",
            "--stdin and --stdin-script both consume stdin",
        );
        return ExitCode::from(2);
    }

    let stdin_input = match read_stdin_input(read_stdin) {
        Ok(input) => input,
        Err(err) => {
            emit_json_error("failed to read stdin", err.to_string());
            return ExitCode::from(1);
        }
    };

    let explicit_frontend = if stone {
        Some(FrontendKind::Stone)
    } else if nu {
        Some(FrontendKind::Nu)
    } else {
        None
    };
    let default_inline_frontend = if eval_mode {
        FrontendKind::Stone
    } else {
        FrontendKind::Nu
    };

    let execution = if args.first().is_some_and(|arg| arg == "-c") {
        args.remove(0);
        if args.is_empty() {
            emit_json_error("usage error", usage(&program_name));
            return ExitCode::from(2);
        }

        Ok((
            explicit_frontend.unwrap_or(default_inline_frontend),
            args.join(" "),
        ))
    } else if read_stdin_script {
        read_stdin_source()
            .map(|source| (explicit_frontend.unwrap_or(FrontendKind::Stone), source))
            .map_err(|err| format!("failed to read stdin script: {err}"))
    } else if args.len() == 1 {
        let path = &args[0];
        read_script_file(path)
            .map(|source| {
                (
                    explicit_frontend.unwrap_or_else(|| frontend_for_script_path(path)),
                    source,
                )
            })
            .map_err(|err| format!("failed to read script file: {err}"))
    } else {
        emit_json_error("usage error", usage(&program_name));
        return ExitCode::from(2);
    };

    let (frontend, source) = match execution {
        Ok(execution) => execution,
        Err(err) => {
            emit_json_error("usage error", err);
            return ExitCode::from(2);
        }
    };

    let code = guest.run_command_with_frontend(frontend, &source, stdin_input);

    ExitCode::from(normalize_exit_code(code))
}

struct StdioTaskStream<'a, R, W> {
    reader: &'a mut R,
    writer: &'a mut W,
}

impl<R: Read, W> Read for StdioTaskStream<'_, R, W> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reader.read(buf)
    }
}

impl<R, W: Write> Write for StdioTaskStream<'_, R, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writer.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

fn normalize_exit_code(code: i32) -> u8 {
    code.rem_euclid(256) as u8
}

fn emit_json_error(error: impl Into<String>, message: impl Into<String>) {
    eprintln!(
        "{}",
        json!({
            "ok": false,
            "error": {
                "message": error.into(),
                "detail": message.into(),
            }
        })
    );
}

fn emit_json_response(response: serde_json::Value, ok: bool) {
    let encoded = serde_json::to_string(&response).unwrap_or_else(|err| {
        format!(
            "{{\"ok\":false,\"error\":{{\"message\":\"failed to encode response\",\"debug\":{:?}}}}}",
            err.to_string()
        )
    });

    if ok {
        println!("{encoded}");
    } else {
        eprintln!("{encoded}");
    }
}

fn read_stdin_input(read_stdin: bool) -> io::Result<nu_protocol::PipelineData> {
    if !read_stdin {
        return Ok(nu_protocol::PipelineData::empty());
    }

    let stdin = io::stdin();
    let mut bytes = Vec::new();
    stdin.lock().read_to_end(&mut bytes)?;
    Ok(pipeline_input_from_bytes(bytes))
}

fn read_stdin_source() -> io::Result<String> {
    let stdin = io::stdin();
    let mut source = String::new();
    stdin.lock().read_to_string(&mut source)?;
    Ok(source)
}

fn read_script_file(path: &str) -> io::Result<String> {
    fs::read_to_string(path)
}

fn frontend_for_script_path(path: &str) -> FrontendKind {
    if Path::new(path)
        .extension()
        .is_some_and(|extension| extension == "stone")
    {
        FrontendKind::Stone
    } else {
        FrontendKind::Nu
    }
}

fn remove_flag(args: &mut Vec<String>, flag: &str) -> bool {
    if let Some(index) = args.iter().position(|arg| arg == flag) {
        args.remove(index);
        true
    } else {
        false
    }
}

fn remove_optional_value(args: &mut Vec<String>, flag: &str, default: &str) -> Option<String> {
    let index = args.iter().position(|arg| arg == flag)?;
    args.remove(index);

    if args.get(index).is_some_and(|value| !value.starts_with('-')) {
        Some(args.remove(index))
    } else {
        Some(default.to_string())
    }
}

fn strip_trailing_hermit_boot_args(args: &mut Vec<String>) {
    if !args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "--task" | "--task-server" | "--task-server-vsock"
        )
    }) {
        return;
    }

    while args
        .last()
        .is_some_and(|arg| is_trailing_hermit_boot_arg(arg))
    {
        args.pop();
    }
}

fn is_trailing_hermit_boot_arg(arg: &str) -> bool {
    matches!(arg, "pci=off" | "pci=on")
        || arg.starts_with("virtio_mmio.device=")
        || arg.starts_with("console=")
        || arg.starts_with("reboot=")
        || arg.starts_with("panic=")
}

fn usage(program_name: &str) -> String {
    format!(
        "usage: {program_name} eval [--stdin] [--stone|--nu] -c <command> | \
         {program_name} eval [--stdin] [--stone|--nu] <script.stone|script.nu> | \
         {program_name} eval [--stone|--nu] --stdin-script | \
         {program_name} help | \
         {program_name} --task [task.json] | \
         {program_name} --task-server | \
         {program_name} --task-server-vsock [port]"
    )
}

#[cfg(test)]
mod tests {
    use super::{
        frontend_for_script_path, is_trailing_hermit_boot_arg, normalize_exit_code, remove_flag,
        remove_optional_value, strip_trailing_hermit_boot_args, usage,
    };
    use waymark_runtime::FrontendKind;

    #[test]
    fn normalizes_exit_codes_to_process_status_range() {
        assert_eq!(normalize_exit_code(0), 0);
        assert_eq!(normalize_exit_code(255), 255);
        assert_eq!(normalize_exit_code(256), 0);
        assert_eq!(normalize_exit_code(257), 1);
        assert_eq!(normalize_exit_code(-1), 255);
        assert_eq!(normalize_exit_code(-257), 255);
    }

    #[test]
    fn detects_frontend_from_script_extension() {
        assert_eq!(
            frontend_for_script_path("script.stone"),
            FrontendKind::Stone
        );
        assert_eq!(frontend_for_script_path("/tmp/script.nu"), FrontendKind::Nu);
        assert_eq!(frontend_for_script_path("/tmp/script"), FrontendKind::Nu);
        assert_eq!(frontend_for_script_path("/tmp/STONE"), FrontendKind::Nu);
    }

    #[test]
    fn removes_flags_in_place() {
        let mut args = vec!["--stdin".to_string(), "-c".to_string(), "1 + 1".to_string()];

        assert!(remove_flag(&mut args, "--stdin"));
        assert!(!remove_flag(&mut args, "--stdin"));
        assert_eq!(args, ["-c", "1 + 1"]);
    }

    #[test]
    fn removes_optional_flag_values_with_defaults() {
        let mut args = vec![
            "--task-server-vsock".to_string(),
            "1234".to_string(),
            "--stone".to_string(),
        ];

        assert_eq!(
            remove_optional_value(&mut args, "--task-server-vsock", "9975"),
            Some("1234".to_string())
        );
        assert_eq!(args, ["--stone"]);

        let mut args = vec!["--task".to_string(), "--stone".to_string()];
        assert_eq!(
            remove_optional_value(&mut args, "--task", "/work/task/task.json"),
            Some("/work/task/task.json".to_string())
        );
        assert_eq!(args, ["--stone"]);

        assert_eq!(
            remove_optional_value(&mut args, "--missing", "default"),
            None
        );
    }

    #[test]
    fn strips_loader_appended_boot_args_from_task_server_mode() {
        let mut args = vec![
            "--task-server-vsock".to_string(),
            "9975".to_string(),
            "pci=off".to_string(),
            "virtio_mmio.device=4K@0xc0001000:5".to_string(),
        ];

        strip_trailing_hermit_boot_args(&mut args);

        assert_eq!(args, ["--task-server-vsock", "9975"]);
    }

    #[test]
    fn strips_loader_appended_boot_args_from_task_file_mode() {
        let mut args = vec![
            "--task".to_string(),
            "/tmp/task.json".to_string(),
            "console=ttyS0".to_string(),
            "reboot=k".to_string(),
            "panic=abort".to_string(),
            "pci=on".to_string(),
        ];

        strip_trailing_hermit_boot_args(&mut args);

        assert_eq!(args, ["--task", "/tmp/task.json"]);
    }

    #[test]
    fn leaves_command_args_alone() {
        let mut args = vec!["-c".to_string(), "echo pci=off".to_string()];

        strip_trailing_hermit_boot_args(&mut args);

        assert_eq!(args, ["-c", "echo pci=off"]);
    }

    #[test]
    fn detects_trailing_hermit_boot_arg_shapes() {
        assert!(is_trailing_hermit_boot_arg("pci=off"));
        assert!(is_trailing_hermit_boot_arg("pci=on"));
        assert!(is_trailing_hermit_boot_arg(
            "virtio_mmio.device=4K@0xc0001000:5"
        ));
        assert!(is_trailing_hermit_boot_arg("console=ttyS0"));
        assert!(is_trailing_hermit_boot_arg("reboot=k"));
        assert!(is_trailing_hermit_boot_arg("panic=abort"));
        assert!(!is_trailing_hermit_boot_arg("panic"));
        assert!(!is_trailing_hermit_boot_arg("user_arg=pci=off"));
    }

    #[test]
    fn usage_mentions_all_execution_modes() {
        let text = usage("waymark-runtime");

        assert!(text.contains("waymark-runtime eval"));
        assert!(text.contains("--stdin-script"));
        assert!(text.contains("--task [task.json]"));
        assert!(text.contains("--task-server"));
        assert!(text.contains("--task-server-vsock [port]"));
    }
}
