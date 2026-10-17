use crate::services::cron::{backup_task_name, stale_backup_tasks};
use std::collections::HashSet;

const ID_A: &str = "16678159-ff7e-4c97-8c83-0adeff214681";
const ID_B: &str = "16678124-ff7e-4c97-8c83-0adeff214681";

fn key(id: &str) -> String {
    format!("redbeat:{}", backup_task_name(id))
}

#[test]
fn backup_task_name_format() {
    assert_eq!(backup_task_name(ID_A), format!("periodic.backup_{ID_A}"));
}

#[test]
fn stale_backup_tasks_returns_ids_missing_from_response() {
    let live: HashSet<&str> = [ID_A].into();
    assert_eq!(
        stale_backup_tasks(&[key(ID_A), key(ID_B)], &live),
        vec![backup_task_name(ID_B)]
    );
}

#[test]
fn stale_backup_tasks_empty_response_prunes_all() {
    assert_eq!(
        stale_backup_tasks(&[key(ID_A), key(ID_B)], &HashSet::new()),
        vec![backup_task_name(ID_A), backup_task_name(ID_B)]
    );
}

#[test]
fn stale_backup_tasks_ignores_other_keys() {
    let keys = vec![
        "redbeat:periodic.other_x".to_string(),
        "periodic.backup_raw".to_string(),
    ];
    assert!(stale_backup_tasks(&keys, &HashSet::new()).is_empty());
}
