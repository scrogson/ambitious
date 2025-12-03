//! Room supervisor module.
//!
//! Uses DynamicSupervisor to manage chat room processes dynamically.
//! Rooms are started on demand and supervised for fault tolerance.

use crate::room::{Room, RoomInit};
use starlang::gen_server;
use starlang::Pid;
use starlang_supervisor::dynamic_supervisor::{self, DynamicSupervisorOpts};
use starlang_supervisor::{ChildSpec, RestartType, StartChildError};
use std::sync::OnceLock;

/// Global room supervisor PID.
static ROOM_SUPERVISOR: OnceLock<Pid> = OnceLock::new();

/// The name used to register the room supervisor.
pub const NAME: &str = "room_supervisor";

/// Starts the room supervisor.
///
/// This should be called once during application startup.
pub async fn start() -> Result<Pid, starlang_supervisor::StartError> {
    let opts = DynamicSupervisorOpts::new()
        .max_restarts(10)
        .max_seconds(5);

    let pid = dynamic_supervisor::start(opts).await?;

    ROOM_SUPERVISOR
        .set(pid)
        .map_err(|_| starlang_supervisor::StartError::InitFailed("already started".to_string()))?;

    Ok(pid)
}

/// Returns the room supervisor PID.
#[allow(dead_code)]
pub fn pid() -> Option<Pid> {
    ROOM_SUPERVISOR.get().copied()
}

/// Starts a new room under the supervisor.
///
/// # Arguments
///
/// * `name` - The room name
///
/// # Returns
///
/// The PID of the started room GenServer.
pub async fn start_room(name: String) -> Result<Pid, StartChildError> {
    let sup_pid = ROOM_SUPERVISOR
        .get()
        .copied()
        .ok_or_else(|| StartChildError::Failed("room supervisor not started".to_string()))?;

    let spec = ChildSpec::new(format!("room:{}", name), move || {
        let room_name = name.clone();
        async move {
            gen_server::start::<Room>(RoomInit { name: room_name })
                .await
                .map_err(|e| StartChildError::Failed(e.to_string()))
        }
    })
    .restart(RestartType::Transient); // Only restart on abnormal exit

    dynamic_supervisor::start_child(sup_pid, spec).await
}

/// Terminates a room by PID.
pub fn terminate_room(room_pid: Pid) -> Result<(), String> {
    let sup_pid = ROOM_SUPERVISOR
        .get()
        .copied()
        .ok_or_else(|| "room supervisor not started".to_string())?;

    dynamic_supervisor::terminate_child(sup_pid, room_pid)
}

/// Returns the count of active rooms.
#[allow(dead_code)]
pub fn count_rooms() -> Result<usize, String> {
    let sup_pid = ROOM_SUPERVISOR
        .get()
        .copied()
        .ok_or_else(|| "room supervisor not started".to_string())?;

    Ok(dynamic_supervisor::count_children(sup_pid)?.active)
}
