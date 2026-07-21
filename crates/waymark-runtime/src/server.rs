// SPDX-License-Identifier: MIT OR Apache-2.0

use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Read, Seek, Write};
use std::path::Path;
use std::thread;

use serde_json::{json, Value as JsonValue};

use crate::agent::{AgentError, AgentModelGateway};
use crate::global_state::{ProcessLocalState, ProcessTls};
use crate::{StoneGuest, VmSupervisor};

const MAX_FRAME_LEN: u32 = 16 * 1024 * 1024;
const FRAME_WRITE_CHUNK_LEN: usize = 2048;
const DEBUG_TLS_BYTES: usize = 1024 * 1024;
#[cfg(all(target_os = "hermit", debug_assertions))]
const ALLOC_SITE_LIVE_LIMIT: usize = 16384;

#[derive(Default)]
struct DebugTlsPayload(Vec<u8>);

// SAFETY: the diagnostic byte buffer is owned by and dropped with its worker
// thread. It never publishes a reference outside that thread.
unsafe impl ProcessLocalState for DebugTlsPayload {}

thread_local! {
    static DEBUG_TLS_PAYLOAD: ProcessTls<DebugTlsPayload> =
        const { ProcessTls::new(DebugTlsPayload(Vec::new())) };
}

pub fn run_task_server<R, W>(
    guest: &mut StoneGuest,
    reader: &mut R,
    writer: &mut W,
) -> io::Result<()>
where
    R: Read,
    W: Write,
{
    while let Some(frame) = read_frame(reader)? {
        let response = handle_frame(guest, &frame);
        write_frame(writer, &response)?;

        if frame
            .get("type")
            .and_then(JsonValue::as_str)
            .is_some_and(|message_type| message_type == "shutdown")
        {
            break;
        }
    }

    Ok(())
}

pub fn run_task_server_stream<S>(guest: &mut StoneGuest, stream: &mut S) -> io::Result<()>
where
    S: Read + Write,
{
    while let Some(frame) = read_frame(stream)? {
        let response = if frame
            .get("type")
            .and_then(JsonValue::as_str)
            .is_some_and(|message_type| message_type == "task")
        {
            task_frame_response_stream(guest, stream, &frame)?
        } else {
            handle_frame(guest, &frame)
        };
        write_frame(stream, &response)?;

        if frame
            .get("type")
            .and_then(JsonValue::as_str)
            .is_some_and(|message_type| message_type == "shutdown")
        {
            break;
        }
    }

    Ok(())
}

/// Serve task frames by creating a fresh Stone root process for every task.
///
/// The vsock connection and `VmSupervisor` remain in the control domain. Model,
/// workspace, and Linux requests borrow that connection only for the lifetime
/// of the scoped Stone worker and cannot be retained by the process.
pub fn run_supervisor_task_server_stream<S>(
    supervisor: &mut VmSupervisor,
    stream: &mut S,
) -> io::Result<()>
where
    S: Read + Write + Send,
{
    while let Some(frame) = read_frame(stream)? {
        let frame_type = frame.get("type").and_then(JsonValue::as_str);
        let response = match frame_type {
            Some("task") => supervisor_task_frame_response_stream(supervisor, stream, &frame)?,
            Some("lease") => supervisor_lease_frame_response(supervisor, &frame),
            Some("hello") => json!({
                "version": 0,
                "type": "hello",
                "features": {
                    "payload_encodings": ["json-inline"],
                    "single_flight": true,
                    "process_model": "fresh-root",
                    "gateway_provider_leases": true,
                    "allocation_domains": cfg!(target_os = "hermit"),
                    "cleanliness_report": true,
                }
            }),
            Some("ping") => json!({
                "version": 0,
                "type": "pong",
            }),
            Some("shutdown") => json!({
                "version": 0,
                "type": "shutdown_ack",
                "vm_state": vm_state_name(supervisor.state()),
            }),
            Some(message_type) => protocol_error(
                frame_id(&frame),
                format!("unsupported supervisor message type `{message_type}`"),
            ),
            None => protocol_error(frame_id(&frame), "request requires type"),
        };
        write_frame(stream, &response)?;

        if frame_type == Some("shutdown") {
            break;
        }
    }

    Ok(())
}

fn supervisor_task_frame_response_stream<S>(
    supervisor: &mut VmSupervisor,
    stream: &mut S,
    frame: &JsonValue,
) -> io::Result<JsonValue>
where
    S: Read + Write + Send,
{
    let Some(id) = frame_id(frame) else {
        return Ok(protocol_error(None, "task request requires id"));
    };
    if frame.get("version").and_then(JsonValue::as_u64) != Some(0) {
        return Ok(protocol_error(Some(id), "request requires version=0"));
    }
    let Some(payload) = frame.get("payload").and_then(JsonValue::as_object) else {
        return Ok(protocol_error(
            Some(id),
            "task request requires object payload",
        ));
    };
    match payload.get("encoding").and_then(JsonValue::as_str) {
        Some("json-inline") => {}
        Some(encoding) => {
            return Ok(protocol_error(
                Some(id),
                format!("unsupported payload encoding `{encoding}`"),
            ))
        }
        None => return Ok(protocol_error(Some(id), "payload requires encoding")),
    }
    let Some(task) = payload.get("task") else {
        return Ok(protocol_error(
            Some(id),
            "json-inline payload requires task",
        ));
    };
    if task.get("id").and_then(JsonValue::as_str) != Some(id) {
        return Ok(protocol_error(Some(id), "task id must match request id"));
    }

    let memory_before = supervisor_memory_stats(supervisor);
    let (dispatch, effects) = if crate::gateway_runtime::enabled() {
        (
            supervisor.dispatch_task(task.clone()),
            StreamEffectCounts::default().to_json("attached_gateway"),
        )
    } else {
        let mut gateway = StreamModelGateway {
            stream,
            task_id: id.to_owned(),
            next_seq: 0,
            effects: StreamEffectCounts::default(),
        };
        let dispatch = supervisor.dispatch_task_with_model_gateway(task.clone(), &mut gateway);
        (dispatch, gateway.effects.to_json("host_stream"))
    };
    let memory_after = supervisor_memory_stats(supervisor);

    Ok(supervisor_dispatch_response(
        supervisor,
        id,
        dispatch,
        effects,
        memory_before,
        memory_after,
    ))
}

fn supervisor_lease_frame_response(supervisor: &mut VmSupervisor, frame: &JsonValue) -> JsonValue {
    let Some(id) = frame_id(frame) else {
        return protocol_error(None, "lease request requires id");
    };
    if frame.get("version").and_then(JsonValue::as_u64) != Some(0) {
        return protocol_error(Some(id), "request requires version=0");
    }
    let Some(payload) = frame.get("payload").and_then(JsonValue::as_object) else {
        return protocol_error(Some(id), "lease request requires object payload");
    };
    if payload.get("encoding").and_then(JsonValue::as_str) != Some("gateway-provider-lease-v0") {
        return protocol_error(
            Some(id),
            "lease payload requires encoding=gateway-provider-lease-v0",
        );
    }
    let Some(gateway) = payload.get("gateway").and_then(JsonValue::as_object) else {
        return protocol_error(Some(id), "lease payload requires gateway object");
    };
    if gateway.get("transport").and_then(JsonValue::as_str) != Some("vsock") {
        return protocol_error(Some(id), "provider lease requires gateway transport=vsock");
    }
    let required_string = |field: &str| {
        gateway
            .get(field)
            .and_then(JsonValue::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("provider lease gateway requires {field}"))
    };
    let required_u32 = |field: &str| {
        gateway
            .get(field)
            .and_then(JsonValue::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| format!("provider lease gateway requires positive u32 {field}"))
    };
    let config = (|| {
        Ok::<_, String>(crate::gateway_runtime::GatewayRuntimeConfig {
            endpoint: crate::gateway_runtime::GatewayEndpoint::Vsock {
                cid: required_u32("cid")?,
                port: required_u32("port")?,
            },
            attempt: String::new(),
            controller_run: String::new(),
            boot_token: required_string("boot_token")?,
            channel_id: String::new(),
            channel_epoch: required_string("channel_epoch")?,
            provider_lease_id: required_string("provider_lease_id")?,
            provider_vm_id: required_string("provider_vm_id")?,
            tx: String::new(),
            image: String::new(),
            container: None,
            workspace_mount: "/app".to_string(),
            host_workspace_path: None,
            capability_profile: String::new(),
            model_class: String::new(),
            control: None,
        })
    })();
    let config = match config {
        Ok(config) => config,
        Err(message) => return protocol_error(Some(id), message),
    };
    let touch_nu_quoting_tls = payload
        .get("diagnostics")
        .and_then(|value| value.get("touch_nu_quoting_tls"))
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    let effects = json!({
        "transport": "gateway_provider_lease",
        "provider_lease_id": config.provider_lease_id.clone(),
        "provider_vm_id": config.provider_vm_id.clone(),
    });
    let memory_before = supervisor_memory_stats(supervisor);
    let dispatch = supervisor.dispatch_gateway_attempt_lease(config, touch_nu_quoting_tls);
    let memory_after = supervisor_memory_stats(supervisor);
    supervisor_dispatch_response(
        supervisor,
        id,
        dispatch,
        effects,
        memory_before,
        memory_after,
    )
}

fn supervisor_dispatch_response(
    supervisor: &VmSupervisor,
    id: &str,
    dispatch: Result<crate::VmDispatchResult, crate::VmDispatchError>,
    effects: JsonValue,
    memory_before: JsonValue,
    memory_after: JsonValue,
) -> JsonValue {
    match dispatch {
        Ok(dispatch) => {
            let cleanliness = dispatch.cleanliness.to_json();
            let mut result = dispatch.response;
            if let Some(fields) = result.as_object_mut() {
                fields.insert(
                    "vm".to_owned(),
                    json!({
                        "state": vm_state_name(supervisor.state()),
                        "process": {
                            "slot": dispatch.process.slot,
                            "generation": dispatch.process.generation,
                        },
                        "cleanliness": cleanliness,
                        "effects": effects,
                        "memory": {
                            "before": memory_before,
                            "after": memory_after,
                        },
                    }),
                );
            }
            json!({
                "version": 0,
                "type": "result",
                "id": id,
                "result": result,
                "reset": {
                    "ok": dispatch.cleanliness.reusable,
                    "task_state": true,
                    "work": false,
                    "fresh_process": true,
                }
            })
        }
        Err(error) => {
            let cleanliness = error
                .cleanliness
                .as_ref()
                .map(|report| report.to_json())
                .unwrap_or(JsonValue::Null);
            json!({
                "version": 0,
                "type": "result",
                "id": id,
                "result": {
                    "version": 0,
                    "id": id,
                    "ok": false,
                    "kind": "vm_error",
                    "error": {
                        "code": error.code,
                        "message": error.message,
                    },
                    "artifacts": [],
                    "vm": {
                        "state": vm_state_name(supervisor.state()),
                        "cleanliness": cleanliness,
                        "effects": effects,
                        "memory": {
                            "before": memory_before,
                            "after": memory_after,
                        },
                    },
                },
                "reset": {
                    "ok": false,
                    "task_state": false,
                    "work": false,
                    "fresh_process": true,
                }
            })
        }
    }
}

fn supervisor_memory_stats(supervisor: &VmSupervisor) -> JsonValue {
    work_memory_stats(supervisor.start_dir()).unwrap_or_else(|err| {
        json!({
            "source": "unavailable",
            "error": err.to_string(),
        })
    })
}

fn vm_state_name(state: crate::VmLifecycleState) -> &'static str {
    match state {
        crate::VmLifecycleState::Ready => "ready",
        crate::VmLifecycleState::Leased => "leased",
        crate::VmLifecycleState::Draining => "draining",
        crate::VmLifecycleState::Poisoned => "poisoned",
    }
}

struct StreamModelGateway<'a, S> {
    stream: &'a mut S,
    task_id: String,
    next_seq: u64,
    effects: StreamEffectCounts,
}

#[derive(Clone, Copy, Default)]
struct StreamEffectCounts {
    model: u64,
    workspace: u64,
    linux: u64,
}

impl StreamEffectCounts {
    fn to_json(self, transport: &'static str) -> JsonValue {
        json!({
            "transport": transport,
            "model": self.model,
            "workspace": self.workspace,
            "linux": self.linux,
        })
    }
}

impl<S> AgentModelGateway for StreamModelGateway<'_, S>
where
    S: Read + Write,
{
    fn request_model(&mut self, request: &JsonValue) -> Result<JsonValue, AgentError> {
        self.effects.model = self.effects.model.saturating_add(1);
        let request_id = format!("{}:model:{}", self.task_id, self.next_seq);
        self.next_seq += 1;
        write_frame(
            self.stream,
            &json!({
                "version": 0,
                "type": "model_request",
                "id": request_id,
                "task_id": self.task_id,
                "request": request,
            }),
        )
        .map_err(|err| AgentError {
            code: "model_gateway_io",
            message: format!("failed to send model request: {err}"),
        })?;

        let response = read_frame(self.stream).map_err(|err| AgentError {
            code: "model_gateway_io",
            message: format!("failed to read model response: {err}"),
        })?;
        let Some(response) = response else {
            return Err(AgentError {
                code: "model_gateway_closed",
                message: "model gateway closed before response".to_owned(),
            });
        };
        if response.get("type").and_then(JsonValue::as_str) != Some("model_response") {
            return Err(AgentError {
                code: "model_gateway_protocol",
                message: "model gateway returned non-model_response frame".to_owned(),
            });
        }
        if response.get("id").and_then(JsonValue::as_str) != Some(request_id.as_str()) {
            return Err(AgentError {
                code: "model_gateway_protocol",
                message: "model gateway response id did not match request id".to_owned(),
            });
        }
        if response.get("ok").and_then(JsonValue::as_bool) == Some(false) {
            let message = response
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(JsonValue::as_str)
                .unwrap_or("model gateway returned an error")
                .to_owned();
            return Err(AgentError {
                code: "model_gateway_error",
                message,
            });
        }
        response.get("response").cloned().ok_or_else(|| AgentError {
            code: "model_gateway_protocol",
            message: "model_response frame requires response".to_owned(),
        })
    }

    fn request_workspace_rpc(&mut self, request: &JsonValue) -> Result<JsonValue, String> {
        self.effects.workspace = self.effects.workspace.saturating_add(1);
        let request_id = format!("{}:workspace:{}", self.task_id, self.next_seq);
        self.next_seq += 1;
        write_frame(
            self.stream,
            &json!({
                "version": 0,
                "type": "workspace_request",
                "id": request_id,
                "task_id": self.task_id,
                "request": request,
            }),
        )
        .map_err(|err| format!("failed to send workspace request: {err}"))?;

        let response = read_frame(self.stream)
            .map_err(|err| format!("failed to read workspace response: {err}"))?
            .ok_or_else(|| "workspace gateway closed before response".to_owned())?;
        if response.get("type").and_then(JsonValue::as_str) != Some("workspace_response") {
            return Err("workspace gateway returned non-workspace_response frame".to_owned());
        }
        if response.get("id").and_then(JsonValue::as_str) != Some(request_id.as_str()) {
            return Err("workspace response id did not match request id".to_owned());
        }
        response
            .get("response")
            .cloned()
            .ok_or_else(|| "workspace_response frame requires response".to_owned())
    }

    fn request_linux_rpc(&mut self, request: &JsonValue) -> Result<JsonValue, String> {
        self.effects.linux = self.effects.linux.saturating_add(1);
        let request_id = format!("{}:linux:{}", self.task_id, self.next_seq);
        self.next_seq += 1;
        write_frame(
            self.stream,
            &json!({
                "version": 0,
                "type": "linux_request",
                "id": request_id,
                "task_id": self.task_id,
                "request": request,
            }),
        )
        .map_err(|err| format!("failed to send linux request: {err}"))?;

        let response = read_frame(self.stream)
            .map_err(|err| format!("failed to read linux response: {err}"))?
            .ok_or_else(|| "linux gateway closed before response".to_owned())?;
        if response.get("type").and_then(JsonValue::as_str) != Some("linux_response") {
            return Err("linux gateway returned non-linux_response frame".to_owned());
        }
        if response.get("id").and_then(JsonValue::as_str) != Some(request_id.as_str()) {
            return Err("linux response id did not match request id".to_owned());
        }
        if response.get("ok").and_then(JsonValue::as_bool) == Some(false) {
            let message = response
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(JsonValue::as_str)
                .unwrap_or("linux gateway returned an error")
                .to_owned();
            return Err(message);
        }
        response
            .get("response")
            .cloned()
            .ok_or_else(|| "linux_response frame requires response".to_owned())
    }
}

fn handle_frame(guest: &mut StoneGuest, frame: &JsonValue) -> JsonValue {
    match frame.get("type").and_then(JsonValue::as_str) {
        Some("hello") => json!({
            "version": 0,
            "type": "hello",
            "features": {
                "payload_encodings": ["json-inline"],
                "single_flight": true,
                "work_reset": "explicit",
                "diagnostics": ["work_stale_handle", "work_memory_plateau", "task_lifecycle", "thread_tls"]
            }
        }),
        Some("ping") => json!({
            "version": 0,
            "type": "pong",
        }),
        Some("shutdown") => json!({
            "version": 0,
            "type": "shutdown_ack",
        }),
        Some("task") => task_frame_response(guest, frame),
        Some("reset_work_dir") => reset_work_dir_response(guest, frame),
        Some("debug_work_stale_handle") => debug_work_stale_handle_response(guest, frame),
        Some("debug_work_memory_plateau") => debug_work_memory_plateau_response(guest, frame),
        Some("debug_task_lifecycle") => debug_task_lifecycle_response(guest, frame),
        Some("debug_thread_tls") => debug_thread_tls_response(guest, frame),
        Some(message_type) => protocol_error(
            frame_id(frame),
            format!("unsupported message type `{message_type}`"),
        ),
        None => protocol_error(frame_id(frame), "request requires type"),
    }
}

fn task_frame_response(guest: &mut StoneGuest, frame: &JsonValue) -> JsonValue {
    let Some(id) = frame_id(frame) else {
        return protocol_error(None, "task request requires id");
    };

    if frame.get("version").and_then(JsonValue::as_u64) != Some(0) {
        return protocol_error(Some(id), "request requires version=0");
    }

    let Some(payload) = frame.get("payload").and_then(JsonValue::as_object) else {
        return protocol_error(Some(id), "task request requires object payload");
    };

    match payload.get("encoding").and_then(JsonValue::as_str) {
        Some("json-inline") => {}
        Some(encoding) => {
            return protocol_error(
                Some(id),
                format!("unsupported payload encoding `{encoding}`"),
            )
        }
        None => return protocol_error(Some(id), "payload requires encoding"),
    }

    let Some(task) = payload.get("task") else {
        return protocol_error(Some(id), "json-inline payload requires task");
    };

    if task.get("id").and_then(JsonValue::as_str) != Some(id) {
        return protocol_error(Some(id), "task id must match request id");
    }

    let drop_result_before_after_reset = task
        .get("diagnostics")
        .and_then(|diagnostics| diagnostics.get("drop_result_before_after_reset"))
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);

    if let Err(err) = guest.reset_task_state() {
        return reset_error(Some(id), "failed to reset task state before task", err);
    }

    let memory_before_task = task_memory_stats(guest);
    let mut result = if is_gateway_attempt_trigger(task) {
        guest.task_response_from_gateway_attempt()
    } else {
        guest.task_response_from_value(task.clone())
    };
    let memory_after_task = task_memory_stats(guest);

    if let Err(err) = guest.reset_task_state() {
        return reset_error(Some(id), "failed to reset task state after task", err);
    }

    if drop_result_before_after_reset {
        drop(result);
        result = json!({
            "ok": true,
            "diagnostic": {
                "result_dropped_before_after_reset_memory_sample": true,
            },
        });
    }
    let memory_after_task_state_reset = task_memory_stats_with_alloc_sites(guest);

    json!({
        "version": 0,
        "type": "result",
        "id": id,
        "result": result,
        "reset": {
            "ok": true,
            "task_state": true,
            "work": false,
            "memory": {
                "before_task": memory_before_task,
                "after_task": memory_after_task,
                "after_reset": memory_after_task_state_reset
            }
        }
    })
}

fn task_frame_response_stream<S>(
    guest: &mut StoneGuest,
    stream: &mut S,
    frame: &JsonValue,
) -> io::Result<JsonValue>
where
    S: Read + Write,
{
    let Some(id) = frame_id(frame) else {
        return Ok(protocol_error(None, "task request requires id"));
    };

    if frame.get("version").and_then(JsonValue::as_u64) != Some(0) {
        return Ok(protocol_error(Some(id), "request requires version=0"));
    }

    let Some(payload) = frame.get("payload").and_then(JsonValue::as_object) else {
        return Ok(protocol_error(
            Some(id),
            "task request requires object payload",
        ));
    };

    match payload.get("encoding").and_then(JsonValue::as_str) {
        Some("json-inline") => {}
        Some(encoding) => {
            return Ok(protocol_error(
                Some(id),
                format!("unsupported payload encoding `{encoding}`"),
            ))
        }
        None => return Ok(protocol_error(Some(id), "payload requires encoding")),
    }

    let Some(task) = payload.get("task") else {
        return Ok(protocol_error(
            Some(id),
            "json-inline payload requires task",
        ));
    };

    if task.get("id").and_then(JsonValue::as_str) != Some(id) {
        return Ok(protocol_error(Some(id), "task id must match request id"));
    }

    let drop_result_before_after_reset = task
        .get("diagnostics")
        .and_then(|diagnostics| diagnostics.get("drop_result_before_after_reset"))
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);

    if let Err(err) = guest.reset_task_state() {
        return Ok(reset_error(
            Some(id),
            "failed to reset task state before task",
            err,
        ));
    }

    let memory_before_task = task_memory_stats(guest);
    let mut result = if is_gateway_attempt_trigger(task) {
        guest.task_response_from_gateway_attempt()
    } else if crate::gateway_runtime::enabled() {
        guest.task_response_from_value(task.clone())
    } else {
        let mut gateway = StreamModelGateway {
            stream,
            task_id: id.to_owned(),
            next_seq: 0,
            effects: StreamEffectCounts::default(),
        };
        guest.task_response_from_value_with_model_gateway(task.clone(), &mut gateway)
    };
    let memory_after_task = task_memory_stats(guest);

    if let Err(err) = guest.reset_task_state() {
        return Ok(reset_error(
            Some(id),
            "failed to reset task state after task",
            err,
        ));
    }

    if drop_result_before_after_reset {
        drop(result);
        result = json!({
            "ok": true,
            "diagnostic": {
                "result_dropped_before_after_reset_memory_sample": true,
            },
        });
    }
    let memory_after_task_state_reset = task_memory_stats_with_alloc_sites(guest);

    Ok(json!({
        "version": 0,
        "type": "result",
        "id": id,
        "result": result,
        "reset": {
            "ok": true,
            "task_state": true,
            "work": false,
            "memory": {
                "before_task": memory_before_task,
                "after_task": memory_after_task,
                "after_reset": memory_after_task_state_reset
            }
        }
    }))
}

fn is_gateway_attempt_trigger(task: &JsonValue) -> bool {
    task.get("gateway_attempt")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
        || task
            .get("runtime")
            .and_then(|runtime| runtime.get("frontend"))
            .and_then(JsonValue::as_str)
            .is_some_and(|frontend| frontend == "gateway_attempt")
}

fn reset_work_dir_response(guest: &mut StoneGuest, frame: &JsonValue) -> JsonValue {
    let id = frame_id(frame);

    if frame.get("version").and_then(JsonValue::as_u64) != Some(0) {
        return protocol_error(id, "request requires version=0");
    }

    let before = task_memory_stats(guest);
    match guest.reset_work_dir() {
        Ok(()) => json!({
            "version": 0,
            "type": "reset_work_dir",
            "id": id.unwrap_or("unknown"),
            "ok": true,
            "reset": {
                "ok": true,
                "task_state": false,
                "work": true,
                "memory": {
                    "before_reset": before,
                    "after_reset": task_memory_stats_with_alloc_sites(guest)
                }
            }
        }),
        Err(err) => reset_error(id, "failed to reset work dir", err),
    }
}

fn debug_thread_tls_response(guest: &mut StoneGuest, frame: &JsonValue) -> JsonValue {
    let id = frame_id(frame);

    if frame.get("version").and_then(JsonValue::as_u64) != Some(0) {
        return protocol_error(id, "request requires version=0");
    }

    let before = task_memory_stats(guest);
    let worker = thread::Builder::new()
        .name("stone-debug-thread-tls".to_owned())
        .stack_size(1024 * 1024)
        .spawn(|| {
            DEBUG_TLS_PAYLOAD.with(|payload| {
                payload.with_mut(|payload| payload.0 = vec![0x5a; DEBUG_TLS_BYTES]);
            });
            work_memory_stats(Path::new("/work")).unwrap_or_else(|err| {
                json!({
                    "source": "unavailable",
                    "error": err.to_string(),
                })
            })
        });

    let worker_after_touch = match worker {
        Ok(handle) => match handle.join() {
            Ok(stats) => stats,
            Err(_) => {
                return json!({
                    "version": 0,
                    "type": "debug_thread_tls",
                    "id": id.unwrap_or("unknown"),
                    "ok": false,
                    "error": {
                        "message": "debug TLS worker panicked",
                    },
                })
            }
        },
        Err(err) => {
            return json!({
                "version": 0,
                "type": "debug_thread_tls",
                "id": id.unwrap_or("unknown"),
                "ok": false,
                "error": {
                    "message": format!("failed to spawn debug TLS worker: {err}"),
                },
            })
        }
    };

    let after_join = task_memory_stats(guest);
    thread::yield_now();
    let after_yield = task_memory_stats(guest);

    json!({
        "version": 0,
        "type": "debug_thread_tls",
        "id": id.unwrap_or("unknown"),
        "ok": true,
        "report": {
            "tls_bytes": DEBUG_TLS_BYTES,
            "before": before,
            "worker_after_touch": worker_after_touch,
            "after_join": after_join,
            "after_yield": after_yield,
        }
    })
}

fn task_memory_stats(guest: &StoneGuest) -> JsonValue {
    let memory = work_memory_stats(&guest.work_dir).unwrap_or_else(|err| {
        json!({
            "source": "unavailable",
            "error": err.to_string(),
        })
    });

    memory
}

fn task_memory_stats_with_alloc_sites(guest: &StoneGuest) -> JsonValue {
    #[cfg(all(target_os = "hermit", debug_assertions))]
    let mut memory = task_memory_stats(guest);
    #[cfg(not(all(target_os = "hermit", debug_assertions)))]
    let memory = task_memory_stats(guest);

    #[cfg(all(target_os = "hermit", debug_assertions))]
    if let Some(fields) = memory.as_object_mut() {
        let current_task = fields
            .get("alloc_owner_current_task")
            .and_then(JsonValue::as_i64);
        fields.insert(
            "alloc_site_live".to_owned(),
            alloc_site_live_json(current_task),
        );
    }

    memory
}

fn debug_task_lifecycle_response(guest: &mut StoneGuest, frame: &JsonValue) -> JsonValue {
    let id = frame_id(frame);

    if frame.get("version").and_then(JsonValue::as_u64) != Some(0) {
        return protocol_error(id, "request requires version=0");
    }

    let scenario = frame
        .get("scenario")
        .and_then(JsonValue::as_str)
        .unwrap_or("completed");

    match scenario {
        "completed" => {
            guest.debug_register_completed_task_resource("debug-completed-resource");
            let before = task_scope_json(&guest.task_scope_snapshot());
            match guest.reset_task_state() {
                Ok(reset) => {
                    let after = task_scope_json(&guest.task_scope_snapshot());
                    json!({
                        "version": 0,
                        "type": "debug_task_lifecycle",
                        "id": id.unwrap_or("unknown"),
                        "ok": after["live"].as_array().is_some_and(|items| items.is_empty())
                            && after["completed"].as_array().is_some_and(|items| items.is_empty()),
                        "report": {
                            "scenario": scenario,
                            "before": before,
                            "reset": task_scope_json(&reset),
                            "after": after,
                        }
                    })
                }
                Err(err) => reset_error(id, "failed to reset completed task resources", err),
            }
        }
        "live" => {
            guest.debug_register_live_task_resource("debug-live-resource");
            let before = task_scope_json(&guest.task_scope_snapshot());
            let reset_result = guest.reset_task_state();
            let rejected_live = reset_result.is_err();
            let reset_error_detail = reset_result.err().map(|err| err.to_string());
            let after_reject = task_scope_json(&guest.task_scope_snapshot());
            guest.debug_clear_task_resources();
            let after_cleanup = task_scope_json(&guest.task_scope_snapshot());
            json!({
                "version": 0,
                "type": "debug_task_lifecycle",
                "id": id.unwrap_or("unknown"),
                "ok": rejected_live
                    && after_cleanup["live"].as_array().is_some_and(|items| items.is_empty())
                    && after_cleanup["completed"].as_array().is_some_and(|items| items.is_empty()),
                "report": {
                    "scenario": scenario,
                    "before": before,
                    "rejected_live": rejected_live,
                    "reset_error": reset_error_detail,
                    "after_reject": after_reject,
                    "after_cleanup": after_cleanup,
                }
            })
        }
        scenario => protocol_error(
            id,
            format!("unsupported task lifecycle scenario `{scenario}`"),
        ),
    }
}

fn task_scope_json(snapshot: &crate::TaskScopeSnapshot) -> JsonValue {
    json!({
        "live": snapshot.live.clone(),
        "completed": snapshot.completed.clone(),
    })
}

fn debug_work_memory_plateau_response(guest: &mut StoneGuest, frame: &JsonValue) -> JsonValue {
    let id = frame_id(frame);

    if frame.get("version").and_then(JsonValue::as_u64) != Some(0) {
        return protocol_error(id, "request requires version=0");
    }

    let cycles = frame
        .get("cycles")
        .and_then(JsonValue::as_u64)
        .unwrap_or(8)
        .clamp(1, 100) as usize;
    let bytes_per_cycle = frame
        .get("bytes_per_cycle")
        .and_then(JsonValue::as_u64)
        .unwrap_or(256 * 1024)
        .clamp(1, 4 * 1024 * 1024) as usize;

    match probe_work_memory_plateau(guest, cycles, bytes_per_cycle) {
        Ok(report) => {
            let plateau_ok = report
                .get("plateau_ok")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false);
            json!({
                "version": 0,
                "type": "debug_work_memory_plateau",
                "id": id.unwrap_or("unknown"),
                "ok": plateau_ok,
                "report": report,
            })
        }
        Err(err) => reset_error(id, "failed to probe work memory plateau", err),
    }
}

fn probe_work_memory_plateau(
    guest: &mut StoneGuest,
    cycles: usize,
    bytes_per_cycle: usize,
) -> io::Result<JsonValue> {
    guest.reset_work_dir()?;
    let baseline = work_memory_stats(&guest.work_dir)?;
    let baseline_active = stat_u64(&baseline, "active_file_bytes");
    let baseline_stale = stat_u64(&baseline, "stale_file_bytes");
    let baseline_stale_files = stat_u64(&baseline, "stale_files");
    let payload = vec![b'x'; bytes_per_cycle];
    let mut samples = Vec::with_capacity(cycles);
    let mut plateau_ok = true;

    for cycle in 0..cycles {
        let path = guest.work_dir.join(format!("memory-churn-{cycle:04}.bin"));
        fs::write(&path, &payload)?;
        let before_reset = work_memory_stats(&guest.work_dir)?;

        guest.reset_work_dir()?;
        let after_reset = work_memory_stats(&guest.work_dir)?;

        let active_after = stat_u64(&after_reset, "active_file_bytes");
        let stale_after = stat_u64(&after_reset, "stale_file_bytes");
        let stale_files_after = stat_u64(&after_reset, "stale_files");

        if active_after != baseline_active
            || stale_after != baseline_stale
            || stale_files_after != baseline_stale_files
        {
            plateau_ok = false;
        }

        samples.push(json!({
            "cycle": cycle,
            "before_reset": before_reset,
            "after_reset": after_reset,
        }));
    }

    guest.reset_work_dir()?;
    let final_stats = work_memory_stats(&guest.work_dir)?;

    Ok(json!({
        "plateau_ok": plateau_ok,
        "cycles": cycles,
        "bytes_per_cycle": bytes_per_cycle,
        "baseline": baseline,
        "final": final_stats,
        "samples": samples,
    }))
}

fn stat_u64(stats: &JsonValue, name: &str) -> u64 {
    stats
        .get(name)
        .and_then(JsonValue::as_u64)
        .unwrap_or(u64::MAX)
}

#[cfg(target_os = "hermit")]
fn work_memory_stats(_work_dir: &std::path::Path) -> io::Result<JsonValue> {
    let mut stats = hermit_abi::mem_stats::default();
    // SAFETY: `stats` is valid writable storage for the duration of the syscall.
    // The kernel writes a plain `repr(C)` value and does not retain the pointer.
    let status = unsafe { hermit_abi::mem_stats(&mut stats) };
    if status < 0 {
        return Err(io::Error::from_raw_os_error(-status));
    }

    #[allow(unused_mut)]
    let mut memory = json!({
        "source": "hermit-memfs",
        "active_file_bytes": stats.active_file_bytes,
        "stale_file_bytes": stats.stale_file_bytes,
        "stale_files": stats.stale_files,
        "allocation_count": stats.allocation_count,
        "allocated_bytes": stats.allocated_bytes,
        "available_bytes": stats.available_bytes,
        "fragment_count": stats.fragment_count,
        "heap_count": stats.heap_count,
        "claimed_bytes": stats.claimed_bytes,
        "scheduler_live_tasks": stats.scheduler_live_tasks,
        "scheduler_task_handles": stats.scheduler_task_handles,
        "scheduler_waiting_entries": stats.scheduler_waiting_entries,
        "scheduler_finished_tasks": stats.scheduler_finished_tasks,
        "scheduler_current_task": stats.scheduler_current_task,
        "scheduler_fpu_owner_task": stats.scheduler_fpu_owner_task,
        "alloc_domain_current": stats.alloc_domain_current,
        "alloc_domain_current_bytes": stats.alloc_domain_current_bytes,
        "alloc_domain_current_count": stats.alloc_domain_current_count,
        "alloc_domain_top_domains": stats.alloc_domain_top_domains,
        "alloc_domain_top_bytes": stats.alloc_domain_top_bytes,
        "alloc_domain_top_counts": stats.alloc_domain_top_counts,
        "alloc_owner_current_task": stats.alloc_owner_current_task,
        "alloc_owner_current_bytes": stats.alloc_owner_current_bytes,
        "alloc_owner_current_count": stats.alloc_owner_current_count,
        "alloc_owner_tracked_count": stats.alloc_owner_tracked_count,
        "alloc_owner_tracked_bytes": stats.alloc_owner_tracked_bytes,
        "alloc_owner_dropped_records": stats.alloc_owner_dropped_records,
        "alloc_site_dropped_records": stats.alloc_site_dropped_records,
    });
    insert_alloc_domain_volume_stats(&mut memory, &stats);
    #[cfg(debug_assertions)]
    if let Some(fields) = memory.as_object_mut() {
        fields.insert("alloc_site_top".to_owned(), alloc_site_top_json(&stats));
    }
    Ok(memory)
}

#[cfg(target_os = "hermit")]
fn insert_alloc_domain_volume_stats(memory: &mut JsonValue, stats: &hermit_abi::mem_stats) {
    if let Some(fields) = memory.as_object_mut() {
        for (name, value) in [
            ("alloc_domain_alloc_count", stats.alloc_domain_alloc_count),
            ("alloc_domain_alloc_bytes", stats.alloc_domain_alloc_bytes),
            (
                "alloc_domain_alloc_zeroed_count",
                stats.alloc_domain_alloc_zeroed_count,
            ),
            (
                "alloc_domain_alloc_zeroed_bytes",
                stats.alloc_domain_alloc_zeroed_bytes,
            ),
            (
                "alloc_domain_realloc_count",
                stats.alloc_domain_realloc_count,
            ),
            (
                "alloc_domain_realloc_old_bytes",
                stats.alloc_domain_realloc_old_bytes,
            ),
            (
                "alloc_domain_realloc_new_bytes",
                stats.alloc_domain_realloc_new_bytes,
            ),
            (
                "alloc_domain_realloc_copy_bytes",
                stats.alloc_domain_realloc_copy_bytes,
            ),
            ("alloc_domain_free_count", stats.alloc_domain_free_count),
            ("alloc_domain_free_bytes", stats.alloc_domain_free_bytes),
            (
                "alloc_domain_fallback_count",
                stats.alloc_domain_fallback_count,
            ),
            (
                "alloc_domain_fallback_bytes",
                stats.alloc_domain_fallback_bytes,
            ),
            (
                "alloc_domain_zeroed_page_count",
                stats.alloc_domain_zeroed_page_count,
            ),
            (
                "alloc_domain_zeroed_page_bytes",
                stats.alloc_domain_zeroed_page_bytes,
            ),
            (
                "alloc_domain_object_zero_bytes",
                stats.alloc_domain_object_zero_bytes,
            ),
            (
                "alloc_domain_known_zero_skip_bytes",
                stats.alloc_domain_known_zero_skip_bytes,
            ),
        ] {
            fields.insert(name.to_owned(), JsonValue::from(value));
        }
    }
}

#[cfg(all(target_os = "hermit", debug_assertions))]
fn alloc_site_top_json(stats: &hermit_abi::mem_stats) -> JsonValue {
    JsonValue::Array(
        (0..stats.alloc_site_top_bytes.len())
            .filter(|&index| stats.alloc_site_top_counts[index] != 0)
            .map(|index| {
                json!({
                    "ip0": format!("{:#x}", stats.alloc_site_top_ip0[index]),
                    "ip1": format!("{:#x}", stats.alloc_site_top_ip1[index]),
                    "task": stats.alloc_site_top_tasks[index],
                    "domain": stats.alloc_site_top_domains[index],
                    "bytes": stats.alloc_site_top_bytes[index],
                    "count": stats.alloc_site_top_counts[index],
                })
            })
            .collect(),
    )
}

#[cfg(all(target_os = "hermit", debug_assertions))]
fn alloc_site_live_json(exclude_task: Option<i64>) -> JsonValue {
    let mut sites = vec![hermit_abi::alloc_site_snapshot::default(); ALLOC_SITE_LIVE_LIMIT];
    let mut total_sites = 0;
    // SAFETY: `sites` is valid writable storage for its length, and `total_sites`
    // is valid writable storage for the duration of the syscall.
    let status =
        unsafe { hermit_abi::alloc_site_stats(sites.as_mut_ptr(), sites.len(), &mut total_sites) };
    if status < 0 {
        return json!({
            "error": format!("alloc_site_stats failed with status {status}"),
        });
    }

    let written = total_sites.min(sites.len());
    sites.truncate(written);
    if let Some(exclude_task) = exclude_task {
        sites.retain(|site| site.task != exclude_task);
    }
    sites.sort_by(|left, right| right.bytes.cmp(&left.bytes));

    json!({
        "total_sites": total_sites,
        "returned_sites": sites.len(),
        "truncated": total_sites > written,
        "excluded_task": exclude_task,
        "sites": sites
            .into_iter()
            .map(|site| {
                json!({
                    "ip0": format!("{:#x}", site.ip0),
                    "ip1": format!("{:#x}", site.ip1),
                    "task": site.task,
                    "domain": site.domain,
                    "bytes": site.bytes,
                    "count": site.count,
                })
            })
            .collect::<Vec<_>>(),
    })
}

#[cfg(not(target_os = "hermit"))]
fn work_memory_stats(work_dir: &std::path::Path) -> io::Result<JsonValue> {
    Ok(json!({
        "source": "host-work-dir",
        "active_file_bytes": directory_file_bytes(work_dir)?,
        "stale_file_bytes": 0,
        "stale_files": 0,
    }))
}

#[cfg(not(target_os = "hermit"))]
fn directory_file_bytes(path: &std::path::Path) -> io::Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            total += directory_file_bytes(&entry.path())?;
        } else if metadata.is_file() {
            total += metadata.len();
        }
    }
    Ok(total)
}

fn debug_work_stale_handle_response(guest: &mut StoneGuest, frame: &JsonValue) -> JsonValue {
    let id = frame_id(frame);

    if frame.get("version").and_then(JsonValue::as_u64) != Some(0) {
        return protocol_error(id, "request requires version=0");
    }

    match probe_work_stale_handle(guest) {
        Ok(report) => {
            let stale_handle_invalidated = report
                .get("stale_handle_invalidated")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false);
            let namespace_isolated = report
                .get("namespace_isolated")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false);
            json!({
                "version": 0,
                "type": "debug_work_stale_handle",
                "id": id.unwrap_or("unknown"),
                "ok": stale_handle_invalidated && namespace_isolated,
                "report": report,
            })
        }
        Err(err) => reset_error(id, "failed to probe stale work handle", err),
    }
}

fn probe_work_stale_handle(guest: &mut StoneGuest) -> io::Result<JsonValue> {
    guest.reset_work_dir()?;

    let path = guest.work_dir.join("stale-handle.txt");
    let mut stale = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(true)
        .open(&path)?;
    stale.write_all(b"before reset")?;
    stale.flush()?;

    guest.reset_work_dir()?;

    let stale_write_after_reset = stale.write_all(b" after reset").map(|_| ());
    let stale_flush_after_reset = stale.flush().map(|_| ());
    let stale_seek_after_reset = stale.rewind().map(|_| ());
    let mut stale_contents = String::new();
    let stale_read_after_reset = stale.read_to_string(&mut stale_contents).map(|_| ());
    let stale_metadata_after_reset = stale.metadata().map(|_| ());

    let old_handle_usable = stale_write_after_reset.is_ok()
        || stale_seek_after_reset.is_ok()
        || stale_read_after_reset.is_ok()
        || stale_metadata_after_reset.is_ok();
    let stale_handle_invalidated = !old_handle_usable;

    let namespace_before_fresh_create = fs::read_to_string(&path).ok();
    let stale_path_visible = namespace_before_fresh_create.is_some();

    fs::write(&path, "fresh generation")?;
    let fresh_contents = fs::read_to_string(&path)?;
    let namespace_isolated = !stale_path_visible
        && namespace_before_fresh_create.is_none()
        && fresh_contents == "fresh generation";

    guest.reset_work_dir()?;

    Ok(json!({
        "stale_handle_invalidated": stale_handle_invalidated,
        "namespace_isolated": namespace_isolated,
        "old_handle": {
            "write_after_reset": io_result_debug(&stale_write_after_reset),
            "flush_after_reset": io_result_debug(&stale_flush_after_reset),
            "seek_after_reset": io_result_debug(&stale_seek_after_reset),
            "read_after_reset": io_result_debug(&stale_read_after_reset),
            "metadata_after_reset": io_result_debug(&stale_metadata_after_reset),
            "read_buffer": stale_contents,
        },
        "fresh_namespace": {
            "path_visible_before_fresh_create": stale_path_visible,
            "read_before_fresh_create": namespace_before_fresh_create,
            "read_after_fresh_create": fresh_contents,
        }
    }))
}

fn io_result_debug(result: &io::Result<()>) -> JsonValue {
    match result {
        Ok(()) => json!({ "ok": true }),
        Err(err) => json!({
            "ok": false,
            "kind": format!("{:?}", err.kind()),
            "message": err.to_string(),
        }),
    }
}

fn frame_id(frame: &JsonValue) -> Option<&str> {
    frame.get("id").and_then(JsonValue::as_str)
}

fn protocol_error(id: Option<&str>, message: impl Into<String>) -> JsonValue {
    json!({
        "version": 0,
        "type": "error",
        "id": id.unwrap_or("unknown"),
        "kind": "protocol_error",
        "error": {
            "message": message.into(),
        },
    })
}

fn reset_error(id: Option<&str>, message: &str, err: impl std::fmt::Display) -> JsonValue {
    json!({
        "version": 0,
        "type": "error",
        "id": id.unwrap_or("unknown"),
        "kind": "reset_error",
        "error": {
            "message": message,
            "detail": err.to_string(),
        },
    })
}

fn read_frame<R: Read>(reader: &mut R) -> io::Result<Option<JsonValue>> {
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(err) if err.kind() == ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }

    let len = u32::from_be_bytes(len_buf);
    if len > MAX_FRAME_LEN {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("frame length {len} exceeds maximum {MAX_FRAME_LEN}"),
        ));
    }

    let mut bytes = vec![0u8; len as usize];
    reader.read_exact(&mut bytes)?;
    let frame = serde_json::from_slice(&bytes).map_err(|err| {
        io::Error::new(ErrorKind::InvalidData, format!("invalid JSON frame: {err}"))
    })?;

    Ok(Some(frame))
}

fn write_frame<W: Write>(writer: &mut W, frame: &JsonValue) -> io::Result<()> {
    let bytes = serde_json::to_vec(frame).map_err(|err| {
        io::Error::new(
            ErrorKind::InvalidData,
            format!("failed to encode response frame: {err}"),
        )
    })?;
    let len = u32::try_from(bytes.len()).map_err(|_| {
        io::Error::new(
            ErrorKind::InvalidData,
            format!("response frame length {} exceeds u32", bytes.len()),
        )
    })?;

    write_all_chunked(writer, &len.to_be_bytes())?;
    write_all_chunked(writer, &bytes)?;
    writer.flush()
}

fn write_all_chunked<W: Write>(writer: &mut W, bytes: &[u8]) -> io::Result<()> {
    for chunk in bytes.chunks(FRAME_WRITE_CHUNK_LEN) {
        writer.write_all(chunk)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{run_supervisor_task_server_stream, run_task_server, run_task_server_stream};
    use crate::{StoneGuest, VmSupervisor};
    use serde_json::{json, Value as JsonValue};
    use std::fs;
    use std::io::{Cursor, Read, Write};
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn supervisor_server_uses_fresh_roots_and_survives_task_failure() {
        let mut input = Vec::new();
        push_frame(
            &mut input,
            &json!({
                "version": 0,
                "type": "task",
                "id": "root-one",
                "payload": {
                    "encoding": "json-inline",
                    "task": {
                        "version": 0,
                        "id": "root-one",
                        "runtime": { "frontend": "stone" },
                        "diagnostics": { "touch_nu_quoting_tls": true },
                        "script": { "source": "secret = 41\nemit(secret + 1)" }
                    }
                }
            }),
        );
        push_frame(
            &mut input,
            &json!({
                "version": 0,
                "type": "task",
                "id": "root-two",
                "payload": {
                    "encoding": "json-inline",
                    "task": {
                        "version": 0,
                        "id": "root-two",
                        "runtime": { "frontend": "stone" },
                        "diagnostics": { "touch_nu_quoting_tls": true },
                        "script": { "source": "emit(secret)" }
                    }
                }
            }),
        );
        push_frame(
            &mut input,
            &json!({
                "version": 0,
                "type": "task",
                "id": "root-three",
                "payload": {
                    "encoding": "json-inline",
                    "task": {
                        "version": 0,
                        "id": "root-three",
                        "runtime": { "frontend": "agent" },
                        "diagnostics": { "touch_nu_quoting_tls": true },
                        "agent": {
                            "model": "fixture-model",
                            "task": "Return the fresh-root answer."
                        }
                    }
                }
            }),
        );
        push_frame(
            &mut input,
            &json!({
                "version": 0,
                "type": "model_response",
                "id": "root-three:model:0",
                "ok": true,
                "response": {
                    "ok": true,
                    "text": "{\"actions\":[{\"final\":{\"answer\":\"fresh-root\"}}]}"
                }
            }),
        );
        push_frame(&mut input, &json!({"version": 0, "type": "shutdown"}));

        crate::gateway_runtime::set_config(None);
        let root = temp_dir("supervisor-fresh-roots");
        let mut supervisor = VmSupervisor::new(root.clone());
        let mut stream = Duplex::new(input);

        run_supervisor_task_server_stream(&mut supervisor, &mut stream).expect("server");
        let frames = read_output_frames(&stream.output);

        assert_eq!(frames[0]["result"]["ok"], json!(true));
        assert_eq!(frames[0]["result"]["value"], json!(42));
        assert_eq!(frames[0]["result"]["vm"]["state"], json!("ready"));
        assert_eq!(
            frames[0]["result"]["vm"]["cleanliness"]["exercised"],
            json!(["nu_quoting_tls"])
        );
        assert_eq!(frames[0]["reset"]["fresh_process"], json!(true));

        assert_eq!(frames[1]["result"]["ok"], json!(false));
        assert_eq!(frames[1]["result"]["vm"]["state"], json!("ready"));
        assert_eq!(frames[1]["result"]["vm"]["process"]["generation"], json!(2));
        assert_eq!(
            frames[1]["result"]["vm"]["cleanliness"]["reusable"],
            json!(true)
        );
        assert_eq!(frames[2]["type"], json!("model_request"));
        assert_eq!(frames[2]["id"], json!("root-three:model:0"));
        assert_eq!(frames[3]["result"]["ok"], json!(true));
        assert_eq!(frames[3]["result"]["value"]["answer"], json!("fresh-root"));
        assert_eq!(frames[3]["result"]["vm"]["effects"]["model"], json!(1));
        assert_eq!(frames[3]["result"]["vm"]["process"]["generation"], json!(3));
        assert_eq!(frames[4]["type"], json!("shutdown_ack"));
        assert_eq!(frames[4]["vm_state"], json!("ready"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn task_server_runs_json_inline_task() {
        let mut input = Vec::new();
        push_frame(
            &mut input,
            &json!({
                "version": 0,
                "type": "task",
                "id": "inline",
                "payload": {
                    "encoding": "json-inline",
                    "task": {
                        "version": 0,
                        "id": "inline",
                        "runtime": { "frontend": "stone" },
                        "script": { "source": "get(\"message\")" },
                        "input": { "message": "hello" }
                    }
                }
            }),
        );
        push_frame(&mut input, &json!({"version": 0, "type": "shutdown"}));

        let root = temp_dir("server-inline");
        let mut guest = StoneGuest::new(root.clone()).expect("guest");
        let mut reader = Cursor::new(input);
        let mut output = Vec::new();

        run_task_server(&mut guest, &mut reader, &mut output).expect("server");
        let frames = read_output_frames(&output);

        assert_eq!(frames[0]["type"], json!("result"));
        assert_eq!(frames[0]["id"], json!("inline"));
        assert_eq!(frames[0]["result"]["ok"], json!(true));
        assert_eq!(frames[0]["result"]["value"], json!("hello"));
        assert_eq!(frames[0]["reset"]["ok"], json!(true));
        assert_eq!(frames[0]["reset"]["task_state"], json!(true));
        assert_eq!(frames[0]["reset"]["work"], json!(false));
        assert_eq!(frames[1]["type"], json!("shutdown_ack"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn task_server_rejects_unknown_payload_encoding() {
        let mut input = Vec::new();
        push_frame(
            &mut input,
            &json!({
                "version": 0,
                "type": "task",
                "id": "bad",
                "payload": {
                    "encoding": "shared-region-ref"
                }
            }),
        );

        let root = temp_dir("server-bad-encoding");
        let mut guest = StoneGuest::new(root.clone()).expect("guest");
        let mut reader = Cursor::new(input);
        let mut output = Vec::new();

        run_task_server(&mut guest, &mut reader, &mut output).expect("server");
        let frames = read_output_frames(&output);

        assert_eq!(frames[0]["type"], json!("error"));
        assert_eq!(frames[0]["id"], json!("bad"));
        assert_eq!(frames[0]["kind"], json!("protocol_error"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn task_server_rejects_mismatched_task_id() {
        let mut input = Vec::new();
        push_frame(
            &mut input,
            &json!({
                "version": 0,
                "type": "task",
                "id": "outer",
                "payload": {
                    "encoding": "json-inline",
                    "task": {
                        "version": 0,
                        "id": "inner",
                        "runtime": { "frontend": "stone" },
                        "script": { "source": "echo(\"nope\")" },
                        "input": {}
                    }
                }
            }),
        );

        let root = temp_dir("server-mismatch");
        let mut guest = StoneGuest::new(root.clone()).expect("guest");
        let mut reader = Cursor::new(input);
        let mut output = Vec::new();

        run_task_server(&mut guest, &mut reader, &mut output).expect("server");
        let frames = read_output_frames(&output);

        assert_eq!(frames[0]["type"], json!("error"));
        assert_eq!(frames[0]["id"], json!("outer"));
        assert_eq!(frames[0]["kind"], json!("protocol_error"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn task_server_preserves_work_dir_between_tasks_until_explicit_reset() {
        let root = temp_dir("server-explicit-reset");
        let marker = root.join("leak.txt");
        let mut input = Vec::new();
        push_frame(
            &mut input,
            &json!({
                "version": 0,
                "type": "task",
                "id": "write-marker",
                "payload": {
                    "encoding": "json-inline",
                    "task": {
                        "version": 0,
                        "id": "write-marker",
                        "runtime": { "frontend": "stone" },
                        "script": {
                            "source": format!(
                                "marker = open(\"{}\", \"w\")\nmarker.write(\"marker\")\nnames = []\nfor item in ls(\"{}\"):\n    names.append(item[\"name\"])\nemit(names)",
                                marker.display(),
                                root.display()
                            )
                        },
                        "input": {}
                    }
                }
            }),
        );
        push_frame(
            &mut input,
            &json!({
                "version": 0,
                "type": "task",
                "id": "list-work",
                "payload": {
                    "encoding": "json-inline",
                    "task": {
                        "version": 0,
                        "id": "list-work",
                        "runtime": { "frontend": "stone" },
                        "script": {
                            "source": format!(
                                "names = []\nfor item in ls(\"{}\"):\n    names.append(item[\"name\"])\nemit(names)",
                                root.display()
                            )
                        },
                        "input": {}
                    }
                }
            }),
        );
        push_frame(
            &mut input,
            &json!({
                "version": 0,
                "type": "reset_work_dir",
                "id": "reset-work"
            }),
        );
        push_frame(
            &mut input,
            &json!({
                "version": 0,
                "type": "task",
                "id": "list-after-reset",
                "payload": {
                    "encoding": "json-inline",
                    "task": {
                        "version": 0,
                        "id": "list-after-reset",
                        "runtime": { "frontend": "stone" },
                        "script": {
                            "source": format!(
                                "names = []\nfor item in ls(\"{}\"):\n    names.append(item[\"name\"])\nemit(names)",
                                root.display()
                            )
                        },
                        "input": {}
                    }
                }
            }),
        );

        let mut guest = StoneGuest::new(root.clone()).expect("guest");
        let mut reader = Cursor::new(input);
        let mut output = Vec::new();

        run_task_server(&mut guest, &mut reader, &mut output).expect("server");
        let frames = read_output_frames(&output);

        assert_eq!(frames[0]["type"], json!("result"));
        assert_eq!(frames[0]["result"]["ok"], json!(true));
        assert_eq!(frames[0]["result"]["value"], json!(["leak.txt"]));
        assert_eq!(frames[0]["reset"]["ok"], json!(true));
        assert_eq!(frames[0]["reset"]["task_state"], json!(true));
        assert_eq!(frames[0]["reset"]["work"], json!(false));
        assert_eq!(frames[1]["type"], json!("result"));
        assert_eq!(frames[1]["result"]["ok"], json!(true));
        assert_eq!(frames[1]["result"]["value"], json!(["leak.txt"]));
        assert_eq!(frames[1]["reset"]["ok"], json!(true));
        assert_eq!(frames[1]["reset"]["task_state"], json!(true));
        assert_eq!(frames[1]["reset"]["work"], json!(false));
        assert_eq!(frames[2]["type"], json!("reset_work_dir"));
        assert_eq!(frames[2]["ok"], json!(true));
        assert_eq!(frames[2]["reset"]["work"], json!(true));
        assert_eq!(frames[3]["type"], json!("result"));
        assert_eq!(frames[3]["result"]["ok"], json!(true));
        assert_eq!(frames[3]["result"]["value"], json!([]));
        assert_eq!(frames[3]["reset"]["work"], json!(false));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn task_server_stream_relays_model_request_frames() {
        let mut input = Vec::new();
        push_frame(
            &mut input,
            &json!({
                "version": 0,
                "type": "task",
                "id": "agent-model",
                "payload": {
                    "encoding": "json-inline",
                    "task": {
                        "version": 0,
                        "id": "agent-model",
                        "runtime": { "frontend": "agent" },
                        "agent": {
                            "model": "fixture-model",
                            "task": "Create /work/answer.txt containing hello stream."
                        },
                        "artifacts": [
                            {
                                "name": "answer",
                                "guest_path": "/work/answer.txt",
                                "kind": "text"
                            }
                        ]
                    }
                }
            }),
        );
        push_frame(
            &mut input,
            &json!({
                "version": 0,
                "type": "model_response",
                "id": "agent-model:model:0",
                "ok": true,
                "response": {
                    "ok": true,
                    "text": "{\"actions\":[{\"tool\":\"write\",\"input\":{\"path\":\"/work/answer.txt\",\"content\":\"hello stream\",\"mode\":\"replace\"}},{\"tool\":\"read\",\"input\":{\"path\":\"/work/answer.txt\"}},{\"final\":{\"answer\":\"hello stream\"}}]}"
                }
            }),
        );

        let root = temp_dir("server-agent-model");
        fs::create_dir_all(root.join("work")).expect("create work");
        let mut guest = StoneGuest::new(root.join("work")).expect("guest");
        let mut stream = Duplex::new(input);

        run_task_server_stream(&mut guest, &mut stream).expect("server");
        let frames = read_output_frames(&stream.output);

        assert_eq!(frames[0]["type"], json!("model_request"));
        assert_eq!(frames[0]["id"], json!("agent-model:model:0"));
        assert_eq!(frames[0]["request"]["model"], json!("fixture-model"));
        assert_eq!(
            frames[0]["request"]["messages"][1]["content"],
            json!("Create /work/answer.txt containing hello stream.")
        );
        assert_eq!(frames[1]["type"], json!("result"));
        assert_eq!(frames[1]["result"]["ok"], json!(true));
        assert_eq!(
            frames[1]["result"]["value"]["answer"],
            json!("hello stream")
        );
        assert_eq!(
            frames[1]["result"]["artifacts"][0]["value"],
            json!("hello stream")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn task_server_reports_task_lifecycle_diagnostics() {
        let mut input = Vec::new();
        push_frame(
            &mut input,
            &json!({
                "version": 0,
                "type": "debug_task_lifecycle",
                "id": "completed",
                "scenario": "completed"
            }),
        );
        push_frame(
            &mut input,
            &json!({
                "version": 0,
                "type": "debug_task_lifecycle",
                "id": "live",
                "scenario": "live"
            }),
        );

        let root = temp_dir("server-lifecycle");
        let mut guest = StoneGuest::new(root.clone()).expect("guest");
        let mut reader = Cursor::new(input);
        let mut output = Vec::new();

        run_task_server(&mut guest, &mut reader, &mut output).expect("server");
        let frames = read_output_frames(&output);

        assert_eq!(frames[0]["type"], json!("debug_task_lifecycle"));
        assert_eq!(frames[0]["id"], json!("completed"));
        assert_eq!(frames[0]["ok"], json!(true));
        assert_eq!(frames[0]["report"]["after"]["live"], json!([]));
        assert_eq!(frames[0]["report"]["after"]["completed"], json!([]));

        assert_eq!(frames[1]["type"], json!("debug_task_lifecycle"));
        assert_eq!(frames[1]["id"], json!("live"));
        assert_eq!(frames[1]["ok"], json!(true));
        assert_eq!(frames[1]["report"]["rejected_live"], json!(true));
        assert_eq!(frames[1]["report"]["after_cleanup"]["live"], json!([]));
        assert_eq!(frames[1]["report"]["after_cleanup"]["completed"], json!([]));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn task_server_reports_work_memory_plateau() {
        let mut input = Vec::new();
        push_frame(
            &mut input,
            &json!({
                "version": 0,
                "type": "debug_work_memory_plateau",
                "id": "memory",
                "cycles": 2,
                "bytes_per_cycle": 4096
            }),
        );

        let root = temp_dir("server-memory");
        let mut guest = StoneGuest::new(root.clone()).expect("guest");
        let mut reader = Cursor::new(input);
        let mut output = Vec::new();

        run_task_server(&mut guest, &mut reader, &mut output).expect("server");
        let frames = read_output_frames(&output);

        assert_eq!(frames[0]["type"], json!("debug_work_memory_plateau"));
        assert_eq!(frames[0]["id"], json!("memory"));
        assert_eq!(frames[0]["ok"], json!(true));
        assert_eq!(frames[0]["report"]["plateau_ok"], json!(true));
        assert_eq!(
            frames[0]["report"]["final"]["active_file_bytes"],
            frames[0]["report"]["baseline"]["active_file_bytes"]
        );

        let _ = fs::remove_dir_all(root);
    }

    fn push_frame(output: &mut Vec<u8>, frame: &JsonValue) {
        let bytes = serde_json::to_vec(frame).expect("encode frame");
        output.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        output.extend_from_slice(&bytes);
    }

    fn read_output_frames(bytes: &[u8]) -> Vec<JsonValue> {
        let mut cursor = Cursor::new(bytes);
        let mut frames = Vec::new();

        while (cursor.position() as usize) < bytes.len() {
            let mut len_buf = [0u8; 4];
            std::io::Read::read_exact(&mut cursor, &mut len_buf).expect("read len");
            let len = u32::from_be_bytes(len_buf) as usize;
            let mut frame = vec![0u8; len];
            std::io::Read::read_exact(&mut cursor, &mut frame).expect("read frame");
            frames.push(serde_json::from_slice(&frame).expect("decode frame"));
        }

        frames
    }

    struct Duplex {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Duplex {
        fn new(input: Vec<u8>) -> Self {
            Self {
                input: Cursor::new(input),
                output: Vec::new(),
            }
        }
    }

    impl Read for Duplex {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.input.read(buf)
        }
    }

    impl Write for Duplex {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.output.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let path = Path::new("/tmp").join(format!("stone-server-{label}-{nanos}"));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }
}
