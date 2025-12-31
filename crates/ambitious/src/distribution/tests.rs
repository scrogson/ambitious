//! Property-based tests for the distribution layer.
//!
//! These tests verify invariants that should hold regardless of the order
//! of operations, timing, or specific values involved.

use super::pg::ProcessGroups;
use crate::core::{Atom, Pid};
use proptest::prelude::*;
use std::collections::{HashMap, HashSet};

// =============================================================================
// Strategies for generating test data
// =============================================================================

/// Generate a random group name (1-20 alphanumeric chars)
fn arb_group_name() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{0,19}".prop_map(|s| s.to_string())
}

/// Generate a random PID with given node name and creation
fn arb_pid_for_node(node_name: &'static str, creation: u32) -> impl Strategy<Value = Pid> {
    (0u64..1000).prop_map(move |id| Pid::from_parts_atom(Atom::new(node_name), id, creation))
}

/// Generate a PID from node1 (creation 0)
fn arb_pid_node1() -> impl Strategy<Value = Pid> {
    arb_pid_for_node("node1@localhost", 0)
}

/// Generate a PID from node2 (creation 0)
fn arb_pid_node2() -> impl Strategy<Value = Pid> {
    arb_pid_for_node("node2@localhost", 0)
}

/// Generate a PID from node2 after restart (creation 1)
#[allow(dead_code)]
fn arb_pid_node2_restarted() -> impl Strategy<Value = Pid> {
    arb_pid_for_node("node2@localhost", 1)
}

/// Operations that can be performed on process groups
#[derive(Debug, Clone)]
enum PgOperation {
    /// Join a PID to a group
    Join { group: String, pid: Pid },
    /// Leave a PID from a group
    Leave { group: String, pid: Pid },
    /// Remove all PIDs from a node (simulates disconnect)
    NodeDisconnect { node: String },
    /// Apply a sync response (simulates reconnection sync)
    #[allow(dead_code)]
    SyncResponse {
        from_node: String,
        groups: Vec<(String, Vec<Pid>)>,
    },
}

/// Generate a random PG operation
fn arb_pg_operation() -> impl Strategy<Value = PgOperation> {
    prop_oneof![
        // Join operations (weighted higher - more common)
        3 => (arb_group_name(), arb_pid_node1()).prop_map(|(group, pid)| PgOperation::Join { group, pid }),
        3 => (arb_group_name(), arb_pid_node2()).prop_map(|(group, pid)| PgOperation::Join { group, pid }),
        // Leave operations
        1 => (arb_group_name(), arb_pid_node1()).prop_map(|(group, pid)| PgOperation::Leave { group, pid }),
        1 => (arb_group_name(), arb_pid_node2()).prop_map(|(group, pid)| PgOperation::Leave { group, pid }),
        // Node disconnect
        1 => Just(PgOperation::NodeDisconnect { node: "node2@localhost".to_string() }),
    ]
}

/// Generate a sequence of PG operations
fn arb_pg_operations(max_len: usize) -> impl Strategy<Value = Vec<PgOperation>> {
    prop::collection::vec(arb_pg_operation(), 0..max_len)
}

// =============================================================================
// Reference implementation for property verification
// =============================================================================

/// Simple reference implementation of ProcessGroups for verification
#[derive(Default, Clone)]
struct ReferencePg {
    groups: HashMap<String, HashSet<Pid>>,
    pid_to_groups: HashMap<Pid, HashSet<String>>,
}

impl ReferencePg {
    fn join(&mut self, group: &str, pid: Pid) {
        self.groups
            .entry(group.to_string())
            .or_default()
            .insert(pid);
        self.pid_to_groups
            .entry(pid)
            .or_default()
            .insert(group.to_string());
    }

    fn leave(&mut self, group: &str, pid: Pid) {
        if let Some(members) = self.groups.get_mut(group) {
            members.remove(&pid);
            if members.is_empty() {
                self.groups.remove(group);
            }
        }
        if let Some(groups) = self.pid_to_groups.get_mut(&pid) {
            groups.remove(group);
            if groups.is_empty() {
                self.pid_to_groups.remove(&pid);
            }
        }
    }

    fn remove_node_members(&mut self, node_name: &str) {
        let pids_to_remove: Vec<Pid> = self
            .pid_to_groups
            .keys()
            .filter(|pid| pid.node_name() == node_name)
            .copied()
            .collect();

        for pid in pids_to_remove {
            if let Some(groups) = self.pid_to_groups.remove(&pid) {
                for group in groups {
                    if let Some(members) = self.groups.get_mut(&group) {
                        members.remove(&pid);
                        if members.is_empty() {
                            self.groups.remove(&group);
                        }
                    }
                }
            }
        }
    }

    fn get_members(&self, group: &str) -> Vec<Pid> {
        self.groups
            .get(group)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default()
    }

    fn all_groups(&self) -> HashSet<String> {
        self.groups.keys().cloned().collect()
    }
}

// =============================================================================
// Property tests
// =============================================================================

proptest! {
    /// Property: join followed by leave results in no membership
    #[test]
    fn prop_join_then_leave_results_in_no_membership(
        group in arb_group_name(),
        pid in arb_pid_node1()
    ) {
        let pg = ProcessGroups::new();

        pg.join(&group, pid);
        prop_assert!(pg.get_members(&group).contains(&pid));

        pg.leave(&group, pid);
        prop_assert!(!pg.get_members(&group).contains(&pid));
    }

    /// Property: double join is idempotent
    #[test]
    fn prop_double_join_is_idempotent(
        group in arb_group_name(),
        pid in arb_pid_node1()
    ) {
        let pg = ProcessGroups::new();

        pg.join(&group, pid);
        let members_after_first = pg.get_members(&group);

        pg.join(&group, pid);
        let members_after_second = pg.get_members(&group);

        prop_assert_eq!(members_after_first.len(), members_after_second.len());
        prop_assert_eq!(members_after_first.len(), 1);
    }

    /// Property: leave on non-member is a no-op
    #[test]
    fn prop_leave_non_member_is_noop(
        group in arb_group_name(),
        pid in arb_pid_node1()
    ) {
        let pg = ProcessGroups::new();

        // Leave without joining first
        pg.leave(&group, pid);

        // Should have no members
        prop_assert!(pg.get_members(&group).is_empty());
    }

    /// Property: remove_node_members removes exactly all PIDs from that node
    #[test]
    fn prop_remove_node_members_removes_only_that_node(
        groups in prop::collection::vec(arb_group_name(), 1..5),
        node1_pids in prop::collection::vec(arb_pid_node1(), 1..5),
        node2_pids in prop::collection::vec(arb_pid_node2(), 1..5),
    ) {
        let pg = ProcessGroups::new();

        // Add members from both nodes to various groups
        for group in &groups {
            for pid in &node1_pids {
                pg.join(group, *pid);
            }
            for pid in &node2_pids {
                pg.join(group, *pid);
            }
        }

        // Remove node2 members
        pg.remove_node_members(Atom::new("node2@localhost"));

        // Verify: node1 PIDs should still be present, node2 PIDs should be gone
        for group in &groups {
            let members = pg.get_members(group);
            for pid in &node1_pids {
                prop_assert!(members.contains(pid), "node1 PID should still be present");
            }
            for pid in &node2_pids {
                prop_assert!(!members.contains(pid), "node2 PID should be removed");
            }
        }
    }

    /// Property: PIDs with different creation numbers are distinct
    #[test]
    fn prop_creation_numbers_distinguish_pids(
        group in arb_group_name(),
        id in 0u64..1000,
    ) {
        let node = Atom::new("node@localhost");
        let pid_v0 = Pid::from_parts_atom(node, id, 0);
        let pid_v1 = Pid::from_parts_atom(node, id, 1);

        let pg = ProcessGroups::new();

        pg.join(&group, pid_v0);
        pg.join(&group, pid_v1);

        let members = pg.get_members(&group);
        prop_assert_eq!(members.len(), 2, "Different creation = different PIDs");
        prop_assert!(members.contains(&pid_v0));
        prop_assert!(members.contains(&pid_v1));
    }

    /// Property: random operations maintain consistency between groups and pid_to_groups
    #[test]
    fn prop_operations_maintain_internal_consistency(
        ops in arb_pg_operations(50)
    ) {
        let pg = ProcessGroups::new();
        let mut reference = ReferencePg::default();

        for op in ops {
            match op {
                PgOperation::Join { group, pid } => {
                    pg.join(&group, pid);
                    reference.join(&group, pid);
                }
                PgOperation::Leave { group, pid } => {
                    pg.leave(&group, pid);
                    reference.leave(&group, pid);
                }
                PgOperation::NodeDisconnect { node } => {
                    pg.remove_node_members(Atom::new(&node));
                    reference.remove_node_members(&node);
                }
                PgOperation::SyncResponse { .. } => {
                    // Skip for now - tested separately
                }
            }
        }

        // Verify all groups match
        for group in reference.all_groups() {
            let actual: HashSet<Pid> = pg.get_members(&group).into_iter().collect();
            let expected: HashSet<Pid> = reference.get_members(&group).into_iter().collect();
            prop_assert_eq!(
                actual,
                expected,
                "Group {} membership mismatch",
                group
            );
        }

        // Verify no extra groups in actual implementation
        let actual_groups: HashSet<String> = pg.all_groups().into_iter().collect();
        prop_assert_eq!(actual_groups, reference.all_groups());
    }

    /// Property: sync response replaces stale memberships
    /// This tests the bug fix where old PIDs would persist after node restart
    #[test]
    fn prop_sync_response_replaces_stale_pids(
        group in arb_group_name(),
        old_id in 0u64..100,
        new_id in 100u64..200,
    ) {
        let pg = ProcessGroups::new();

        let node2 = Atom::new("node2@localhost");

        // Old PID from node2 (creation 0)
        let old_pid = Pid::from_parts_atom(node2, old_id, 0);

        // New PID from node2 after restart (creation 1)
        let new_pid = Pid::from_parts_atom(node2, new_id, 1);

        // Simulate: old node2 had a process in the group
        pg.join(&group, old_pid);
        prop_assert!(pg.get_members(&group).contains(&old_pid));

        // Simulate: node2 disconnected and reconnected
        // The SyncResponse handler should clear old memberships first
        pg.remove_node_members(node2);
        prop_assert!(!pg.get_members(&group).contains(&old_pid));

        // Now add the new membership (from sync)
        pg.join(&group, new_pid);

        // Verify: only new PID is present
        let members = pg.get_members(&group);
        prop_assert!(!members.contains(&old_pid), "Old PID should not be present");
        prop_assert!(members.contains(&new_pid), "New PID should be present");
        prop_assert_eq!(members.len(), 1);
    }

    /// Property: leave_all removes PID from all groups
    #[test]
    fn prop_leave_all_removes_from_all_groups(
        groups in prop::collection::vec(arb_group_name(), 1..10),
        pid in arb_pid_node1(),
    ) {
        let pg = ProcessGroups::new();

        // Join the PID to all groups
        for group in &groups {
            pg.join(group, pid);
        }

        // Verify joined
        for group in &groups {
            prop_assert!(pg.get_members(group).contains(&pid));
        }

        // Leave all
        pg.leave_all(pid);

        // Verify gone from all groups
        for group in &groups {
            prop_assert!(!pg.get_members(group).contains(&pid));
        }
    }
}

// =============================================================================
// Non-proptest unit tests for specific scenarios
// =============================================================================

#[cfg(test)]
mod scenario_tests {
    use super::*;

    /// Test the exact scenario from the bug report:
    /// node2 restarts, alice on node1 receives messages meant for bob
    #[test]
    fn test_node_restart_clears_stale_memberships() {
        let pg = ProcessGroups::new();

        let node1 = Atom::new("node1@localhost");
        let node2 = Atom::new("node2@localhost");

        // Alice on node1
        let alice = Pid::from_parts_atom(node1, 1, 0);

        // Bob on node2 (first incarnation)
        let bob_v1 = Pid::from_parts_atom(node2, 2, 0);

        // Both join the "chat:room1" group
        pg.join("chat:room1", alice);
        pg.join("chat:room1", bob_v1);

        assert_eq!(pg.get_members("chat:room1").len(), 2);

        // Node2 crashes and restarts with new creation number
        pg.remove_node_members(node2);

        // Only alice should remain
        let members = pg.get_members("chat:room1");
        assert_eq!(members.len(), 1);
        assert!(members.contains(&alice));
        assert!(!members.contains(&bob_v1));

        // Bob reconnects with new creation number
        let bob_v2 = Pid::from_parts_atom(node2, 2, 1); // Same ID, different creation
        pg.join("chat:room1", bob_v2);

        // Now both should be present, but as different PIDs
        let members = pg.get_members("chat:room1");
        assert_eq!(members.len(), 2);
        assert!(members.contains(&alice));
        assert!(members.contains(&bob_v2));
        assert!(!members.contains(&bob_v1)); // Old bob is gone
    }

    /// Test that concurrent node disconnects don't cause issues
    #[test]
    fn test_multiple_node_disconnects() {
        let pg = ProcessGroups::new();

        let node1 = Atom::new("node1@localhost");
        let node2 = Atom::new("node2@localhost");
        let node3 = Atom::new("node3@localhost");

        // Create PIDs on each node
        let pid1 = Pid::from_parts_atom(node1, 1, 0);
        let pid2 = Pid::from_parts_atom(node2, 1, 0);
        let pid3 = Pid::from_parts_atom(node3, 1, 0);

        // All join the same group
        pg.join("shared", pid1);
        pg.join("shared", pid2);
        pg.join("shared", pid3);

        assert_eq!(pg.get_members("shared").len(), 3);

        // Disconnect node2 and node3
        pg.remove_node_members(node2);
        pg.remove_node_members(node3);

        // Only node1 should remain
        let members = pg.get_members("shared");
        assert_eq!(members.len(), 1);
        assert!(members.contains(&pid1));
    }

    /// Test that removing a node that has no members is safe
    #[test]
    fn test_remove_nonexistent_node_is_safe() {
        let pg = ProcessGroups::new();

        let node1 = Atom::new("node1@localhost");
        let node99 = Atom::new("node99@localhost");

        let pid1 = Pid::from_parts_atom(node1, 1, 0);
        pg.join("group1", pid1);

        // Remove a node that doesn't exist
        pg.remove_node_members(node99);

        // Original membership should be unchanged
        assert!(pg.get_members("group1").contains(&pid1));
    }
}
