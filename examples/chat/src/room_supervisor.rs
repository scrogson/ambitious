//! Room supervisor module.
//!
//! Uses DynamicSupervisor to manage chat room processes dynamically.
//! Rooms are started on demand and supervised for fault tolerance.
//! Each room is registered globally so it's accessible from any node.

use crate::room::{Room, RoomInit};
use starlang::Pid;
use starlang::dist::global;
use starlang::gen_server;
use starlang::supervisor::dynamic_supervisor::{self, DynamicSupervisorOpts};
use starlang::supervisor::{ChildSpec, RestartType, StartChildError, StartError};
use std::sync::OnceLock;

/// Global room supervisor PID.
static ROOM_SUPERVISOR: OnceLock<Pid> = OnceLock::new();

/// The name used to register the room supervisor.
pub const NAME: &str = "room_supervisor";

/// Starts the room supervisor.
///
/// This should be called once during application startup.
pub async fn start() -> Result<Pid, StartError> {
    let opts = DynamicSupervisorOpts::new().max_restarts(10).max_seconds(5);

    let pid = dynamic_supervisor::start(opts).await?;

    ROOM_SUPERVISOR
        .set(pid)
        .map_err(|_| StartError::InitFailed("already started".to_string()))?;

    Ok(pid)
}

/// Returns the room supervisor PID.
#[allow(dead_code)]
pub fn pid() -> Option<Pid> {
    ROOM_SUPERVISOR.get().copied()
}

/// Get the global name for a room.
fn room_global_name(name: &str) -> String {
    format!("room:{}", name)
}

/// Get or create a room by name.
///
/// If the room exists globally (on any node), returns its PID.
/// Otherwise, creates a new room under this node's supervisor and registers it globally.
pub async fn get_or_create_room(name: &str) -> Result<Pid, StartChildError> {
    let global_name = room_global_name(name);

    // Check if room already exists globally
    if let Some(pid) = global::whereis(&global_name) {
        return Ok(pid);
    }

    // Create the room
    let room_pid = start_room(name.to_string()).await?;

    // Try to register globally
    if global::register(&global_name, room_pid) {
        tracing::info!(room = %name, pid = ?room_pid, "Room registered globally");
        Ok(room_pid)
    } else {
        // Another node beat us - use theirs and terminate ours
        if let Some(existing_pid) = global::whereis(&global_name) {
            tracing::info!(room = %name, "Room exists globally, using existing");
            let _ = terminate_room(room_pid);
            Ok(existing_pid)
        } else {
            // Weird state - just return what we created
            Ok(room_pid)
        }
    }
}

/// Get a room by name if it exists.
pub fn get_room(name: &str) -> Option<Pid> {
    let global_name = room_global_name(name);
    global::whereis(&global_name)
}

/// Starts a new room under the supervisor (internal use).
async fn start_room(name: String) -> Result<Pid, StartChildError> {
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
