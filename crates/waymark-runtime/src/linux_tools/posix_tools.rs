// SPDX-License-Identifier: MIT OR Apache-2.0

use std::fs;
use std::path::Path;
use std::time::Duration;

use nu_protocol::{Record, Span, Value};
use sysinfo::{
    Components, CpuRefreshKind, Disks, Networks, System, Users, MINIMUM_CPU_UPDATE_INTERVAL,
};

pub(crate) fn ps_record(interval_ms: u64) -> Value {
    let span = Span::unknown();
    #[cfg(any(
        target_os = "android",
        target_os = "linux",
        target_os = "macos",
        target_os = "windows"
    ))]
    {
        return ps_record_supported(interval_ms, span);
    }

    #[allow(unreachable_code)]
    Value::list(Vec::new(), span)
}

#[cfg(any(
    target_os = "android",
    target_os = "linux",
    target_os = "macos",
    target_os = "windows"
))]
fn ps_record_supported(interval_ms: u64, span: Span) -> Value {
    let interval = Duration::from_millis(interval_ms);
    let cores = std::thread::available_parallelism()
        .map(|cores| cores.get())
        .unwrap_or(1);
    let processes = nu_system::collect_proc(interval, false)
        .into_iter()
        .map(|process| {
            let mut record = Record::with_capacity(10);
            record.push("pid", Value::int(i64::from(process.pid()), span));
            record.push("ppid", Value::int(i64::from(process.ppid()), span));
            record.push("name", Value::string(process.name(), span));
            record.push("command", Value::string(process.command(), span));
            record.push("status", Value::string(process.status(), span));
            record.push("cwd", Value::string(process.cwd(), span));
            record.push(
                "cpu_percent",
                Value::float(process.cpu_usage() / cores as f64, span),
            );
            record.push(
                "memory_bytes",
                Value::int(u64_to_i64_saturating(process.mem_size()), span),
            );
            record.push(
                "virtual_bytes",
                Value::int(u64_to_i64_saturating(process.virtual_size()), span),
            );
            record.push(
                "owner_uid",
                Value::int(i64::from(process.curr_proc.owner()), span),
            );
            Value::record(record, span)
        })
        .collect();

    Value::list(processes, span)
}

pub(crate) fn sysinfo_record(section: Option<&str>) -> Result<Value, String> {
    match section {
        None | Some("all") => Ok(sysinfo_all_record()),
        Some("os" | "host") => Ok(sysinfo_os_record(Span::unknown())),
        Some("cpu") => Ok(sysinfo_cpu_records(false, Span::unknown())),
        Some("cpu_long") | Some("cpu-long") => Ok(sysinfo_cpu_records(true, Span::unknown())),
        Some("mem" | "memory") => Ok(sysinfo_memory_record(Span::unknown())),
        Some("disks" | "disk") => Ok(sysinfo_disk_records(Span::unknown())),
        Some("net" | "network" | "networks") => Ok(sysinfo_network_records(Span::unknown())),
        Some("temp" | "temperature" | "temperatures") => Ok(sysinfo_temperature_records(Span::unknown())),
        Some("users" | "user") => Ok(sysinfo_user_records(Span::unknown())),
        Some(other) => Err(format!(
            "unsupported sysinfo section `{other}`; expected os, cpu, cpu_long, mem, disks, net, temp, users, or all"
        )),
    }
}

fn sysinfo_all_record() -> Value {
    let span = Span::unknown();
    let mut record = Record::with_capacity(7);
    record.push("os", sysinfo_os_record(span));
    record.push("cpu", sysinfo_cpu_records(false, span));
    record.push("mem", sysinfo_memory_record(span));
    record.push("disks", sysinfo_disk_records(span));
    record.push("net", sysinfo_network_records(span));
    record.push("temp", sysinfo_temperature_records(span));
    record.push("users", sysinfo_user_records(span));
    Value::record(record, span)
}

fn sysinfo_os_record(span: Span) -> Value {
    let mut record = Record::with_capacity(13);
    record.push("os", Value::string(std::env::consts::OS, span));
    record.push("arch", Value::string(std::env::consts::ARCH, span));
    record.push("family", Value::string(std::env::consts::FAMILY, span));
    record.push(
        "current_pid",
        Value::int(i64::from(std::process::id()), span),
    );
    record.push(
        "cpu_count",
        Value::int(
            i64::try_from(
                std::thread::available_parallelism()
                    .map(|cores| cores.get())
                    .unwrap_or(1),
            )
            .unwrap_or(i64::MAX),
            span,
        ),
    );
    record.push(
        "hostname",
        optional_string(hostname().filter(|value| !value.is_empty()), span),
    );
    record.push(
        "kernel",
        optional_string(uname_field(UnameField::Release), span),
    );
    record.push(
        "kernel_name",
        optional_string(uname_field(UnameField::Name), span),
    );
    record.push("os_release", os_release_record(span));
    push_optional_string(&mut record, "name", System::name(), span);
    push_optional_string(&mut record, "os_version", System::os_version(), span);
    push_optional_string(
        &mut record,
        "long_os_version",
        System::long_os_version(),
        span,
    );
    push_optional_string(
        &mut record,
        "kernel_version",
        System::kernel_version(),
        span,
    );
    push_optional_string(&mut record, "sysinfo_hostname", System::host_name(), span);
    record.push(
        "uptime_ns",
        Value::int(
            u64_to_i64_saturating(System::uptime().saturating_mul(1_000_000_000)),
            span,
        ),
    );
    record.push(
        "boot_time_unix",
        Value::int(u64_to_i64_saturating(System::boot_time()), span),
    );
    #[cfg(target_family = "unix")]
    record.push("umask", Value::int(i64::from(nu_system::get_umask()), span));
    #[cfg(not(target_family = "unix"))]
    record.push("umask", Value::nothing(span));
    Value::record(record, span)
}

fn sysinfo_cpu_records(long: bool, span: Span) -> Value {
    let mut sys = System::new();
    if long {
        sys.refresh_cpu_specifics(CpuRefreshKind::everything());
        std::thread::sleep(MINIMUM_CPU_UPDATE_INTERVAL * 2);
        sys.refresh_cpu_specifics(CpuRefreshKind::nothing().with_cpu_usage());
    } else {
        sys.refresh_cpu_specifics(CpuRefreshKind::nothing().with_frequency());
    }

    let load = System::load_average();
    let load_average = format!("{:.2}, {:.2}, {:.2}", load.one, load.five, load.fifteen);
    Value::list(
        sys.cpus()
            .iter()
            .map(|cpu| {
                let mut record = Record::with_capacity(if long { 6 } else { 5 });
                record.push("name", Value::string(trim_cstyle_null(cpu.name()), span));
                record.push("brand", Value::string(trim_cstyle_null(cpu.brand()), span));
                record.push(
                    "vendor_id",
                    Value::string(trim_cstyle_null(cpu.vendor_id()), span),
                );
                record.push("freq", Value::int(cpu.frequency() as i64, span));
                record.push("load_average", Value::string(load_average.clone(), span));
                if long {
                    let rounded = (f64::from(cpu.cpu_usage()) * 10.0).round() / 10.0;
                    record.push("cpu_usage", Value::float(rounded, span));
                }
                Value::record(record, span)
            })
            .collect(),
        span,
    )
}

fn sysinfo_memory_record(span: Span) -> Value {
    let mut sys = System::new();
    sys.refresh_memory();
    let mut record = Record::with_capacity(7);
    record.push(
        "total",
        Value::filesize(u64_to_i64_saturating(sys.total_memory()), span),
    );
    record.push(
        "free",
        Value::filesize(u64_to_i64_saturating(sys.free_memory()), span),
    );
    record.push(
        "used",
        Value::filesize(u64_to_i64_saturating(sys.used_memory()), span),
    );
    record.push(
        "available",
        Value::filesize(u64_to_i64_saturating(sys.available_memory()), span),
    );
    record.push(
        "swap_total",
        Value::filesize(u64_to_i64_saturating(sys.total_swap()), span),
    );
    record.push(
        "swap_free",
        Value::filesize(u64_to_i64_saturating(sys.free_swap()), span),
    );
    record.push(
        "swap_used",
        Value::filesize(u64_to_i64_saturating(sys.used_swap()), span),
    );
    Value::record(record, span)
}

fn sysinfo_disk_records(span: Span) -> Value {
    Value::list(
        Disks::new_with_refreshed_list()
            .iter()
            .map(|disk| {
                let mut record = Record::with_capacity(7);
                record.push(
                    "device",
                    Value::string(trim_cstyle_null(disk.name().to_string_lossy()), span),
                );
                record.push(
                    "type",
                    Value::string(trim_cstyle_null(disk.file_system().to_string_lossy()), span),
                );
                record.push(
                    "mount",
                    Value::string(disk.mount_point().to_string_lossy(), span),
                );
                record.push(
                    "total",
                    Value::filesize(u64_to_i64_saturating(disk.total_space()), span),
                );
                record.push(
                    "free",
                    Value::filesize(u64_to_i64_saturating(disk.available_space()), span),
                );
                record.push("removable", Value::bool(disk.is_removable(), span));
                record.push("kind", Value::string(disk.kind().to_string(), span));
                Value::record(record, span)
            })
            .collect(),
        span,
    )
}

fn sysinfo_network_records(span: Span) -> Value {
    Value::list(
        Networks::new_with_refreshed_list()
            .iter()
            .map(|(iface, data)| {
                let ips = data
                    .ip_networks()
                    .iter()
                    .map(|ip| {
                        let protocol = match ip.addr {
                            std::net::IpAddr::V4(_) => "ipv4",
                            std::net::IpAddr::V6(_) => "ipv6",
                        };
                        let mut ip_record = Record::with_capacity(4);
                        ip_record.push("address", Value::string(ip.addr.to_string(), span));
                        ip_record.push("protocol", Value::string(protocol, span));
                        ip_record.push("loop", Value::bool(ip.addr.is_loopback(), span));
                        ip_record.push("multicast", Value::bool(ip.addr.is_multicast(), span));
                        Value::record(ip_record, span)
                    })
                    .collect();
                let mut record = Record::with_capacity(5);
                record.push("name", Value::string(trim_cstyle_null(iface), span));
                record.push("mac", Value::string(data.mac_address().to_string(), span));
                record.push("ip", Value::list(ips, span));
                record.push(
                    "sent",
                    Value::filesize(u64_to_i64_saturating(data.total_transmitted()), span),
                );
                record.push(
                    "recv",
                    Value::filesize(u64_to_i64_saturating(data.total_received()), span),
                );
                Value::record(record, span)
            })
            .collect(),
        span,
    )
}

fn sysinfo_temperature_records(span: Span) -> Value {
    Value::list(
        Components::new_with_refreshed_list()
            .iter()
            .map(|component| {
                let mut record = Record::with_capacity(4);
                record.push("unit", Value::string(component.label(), span));
                record.push(
                    "temp",
                    Value::float(component.temperature().unwrap_or(f32::NAN).into(), span),
                );
                record.push(
                    "high",
                    Value::float(component.max().unwrap_or(f32::NAN).into(), span),
                );
                if let Some(critical) = component.critical() {
                    record.push("critical", Value::float(critical.into(), span));
                }
                Value::record(record, span)
            })
            .collect(),
        span,
    )
}

fn sysinfo_user_records(span: Span) -> Value {
    Value::list(
        Users::new_with_refreshed_list()
            .iter()
            .map(|user| {
                let groups = user
                    .groups()
                    .iter()
                    .map(|group| Value::string(trim_cstyle_null(group.name()), span))
                    .collect();
                #[cfg(windows)]
                let id = Value::string(user.id().to_string(), span);
                #[cfg(not(windows))]
                let id = {
                    let id_ref: &u32 = user.id();
                    Value::int(i64::from(*id_ref), span)
                };
                let mut record = Record::with_capacity(3);
                record.push("id", id);
                record.push("name", Value::string(trim_cstyle_null(user.name()), span));
                record.push("groups", Value::list(groups, span));
                Value::record(record, span)
            })
            .collect(),
        span,
    )
}

fn trim_cstyle_null(value: impl AsRef<str>) -> String {
    value.as_ref().trim_end_matches('\0').to_owned()
}

fn push_optional_string(record: &mut Record, name: &str, value: Option<String>, span: Span) {
    if let Some(value) = value {
        record.push(name, Value::string(trim_cstyle_null(value), span));
    }
}

fn optional_string(value: Option<String>, span: Span) -> Value {
    value.map_or_else(|| Value::nothing(span), |value| Value::string(value, span))
}

fn hostname() -> Option<String> {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| read_trimmed("/proc/sys/kernel/hostname"))
}

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

fn os_release_record(span: Span) -> Value {
    let mut record = Record::new();
    if let Ok(text) = fs::read_to_string("/etc/os-release") {
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if key == "NAME" || key == "VERSION_ID" || key == "ID" || key == "PRETTY_NAME" {
                record.push(
                    key.to_ascii_lowercase(),
                    Value::string(unquote(value), span),
                );
            }
        }
    }
    Value::record(record, span)
}

fn unquote(value: &str) -> String {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
        .replace("\\\"", "\"")
}

fn u64_to_i64_saturating(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

enum UnameField {
    Name,
    Release,
}

#[cfg(unix)]
fn uname_field(field: UnameField) -> Option<String> {
    use std::ffi::CStr;
    use std::mem::MaybeUninit;

    let mut uts = MaybeUninit::<libc::utsname>::uninit();
    // SAFETY: uname initializes the utsname struct on success and does not retain pointers.
    if unsafe { libc::uname(uts.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: uname returned success, so uts is initialized and fields are NUL-terminated.
    let uts = unsafe { uts.assume_init() };
    let bytes = match field {
        UnameField::Name => &uts.sysname,
        UnameField::Release => &uts.release,
    };
    // SAFETY: POSIX uname fields are NUL-terminated character arrays on success.
    Some(
        unsafe { CStr::from_ptr(bytes.as_ptr()) }
            .to_string_lossy()
            .into_owned(),
    )
}

#[cfg(not(unix))]
fn uname_field(_field: UnameField) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::{ps_record, sysinfo_record};
    use nu_protocol::Value;

    #[test]
    fn sys_record_contains_basic_host_fields() {
        let value = sysinfo_record(None).expect("all");
        let Value::Record { val, .. } = value else {
            panic!("expected record");
        };
        assert!(val.get("os").is_some());
        assert!(val.get("cpu").is_some());
        assert!(val.get("mem").is_some());
    }

    #[test]
    fn sysinfo_sections_return_expected_shapes() {
        let Value::Record { val: os, .. } = sysinfo_record(Some("os")).expect("os") else {
            panic!("expected os record");
        };
        assert!(os.get("arch").is_some());
        assert!(os.get("cpu_count").is_some());

        let Value::Record { val: mem, .. } = sysinfo_record(Some("mem")).expect("mem") else {
            panic!("expected mem record");
        };
        assert!(mem.get("total").is_some());

        let Value::List { .. } = sysinfo_record(Some("cpu")).expect("cpu") else {
            panic!("expected cpu list");
        };
    }

    #[test]
    fn ps_record_returns_process_records() {
        let value = ps_record(0);
        let Value::List { vals, .. } = value else {
            panic!("expected list");
        };
        assert!(vals.iter().any(|value| {
            matches!(
                value,
                Value::Record { val, .. } if val.get("pid").is_some() && val.get("name").is_some()
            )
        }));
    }
}
