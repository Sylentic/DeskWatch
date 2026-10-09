//! Reading a build timeline: where a pipeline run is right now.
//!
//! A timeline is a flat list of records that point at their parent with
//! `parentId`: `Stage` > `Phase` > `Job` > `Task`. A stage that is gated by an
//! environment approval holds a `Checkpoint` with a `Checkpoint.Approval`
//! child that stays in progress until someone approves.
//!
//! For a Terraform pipeline the stages are typically `plan` and `apply`, so
//! the panel shows "apply: terraform apply" and a bar that moves with the
//! stages, and "Approval needed" while the apply waits for a person.

use std::collections::HashMap;

use super::payload::Record;

/// What the panel needs to know about one build timeline.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct View {
    /// A stage is waiting for an approval.
    pub waiting_approval: bool,
    /// Current task, with its stage in front unless the stage is the implicit
    /// default one: `apply: terraform apply`.
    pub step: Option<String>,
    /// 1-based position of the current task within its stage.
    pub step_no: Option<u32>,
    /// Number of tasks the current stage has listed so far.
    pub step_count: Option<u32>,
    /// Finished stages plus the finished share of the current stage, over all
    /// stages. 0.0 to 1.0.
    pub fraction: Option<f32>,
    /// First failed task, else the first failed job or stage, for the red alert.
    pub failing_step: Option<String>,
    /// Every stage is completed, so the build result is worth fetching now.
    pub finished: bool,
}

fn is_state(record: &Record, state: &str) -> bool {
    record.state.as_deref() == Some(state)
}

fn is_failed(record: &Record) -> bool {
    record.result.as_deref() == Some("failed")
}

/// The stage a record belongs to, found by walking up `parentId`.
fn stage_of<'a>(by_id: &HashMap<&str, &'a Record>, record: &'a Record) -> Option<&'a Record> {
    let mut current = record;
    for _ in 0..8 {
        if current.kind == "Stage" {
            return Some(current);
        }
        current = by_id.get(current.parent_id.as_deref()?)?;
    }
    None
}

/// Name for the panel: the stage in front, unless it is the implicit one a
/// pipeline without `stages:` gets.
fn label(stage: Option<&Record>, task: &str) -> String {
    match stage {
        Some(s) if !s.name.is_empty() && s.name != "__default" => format!("{}: {task}", s.name),
        _ => task.to_string(),
    }
}

pub fn analyse(records: &[Record]) -> View {
    let by_id: HashMap<&str, &Record> = records.iter().map(|r| (r.id.as_str(), r)).collect();
    let mut stages: Vec<&Record> = records.iter().filter(|r| r.kind == "Stage").collect();
    stages.sort_by_key(|s| s.order.unwrap_or(0));

    // Tasks of a stage, in run order. With no stage records (an odd answer)
    // all tasks count as one stage.
    let tasks_of = |stage: Option<&Record>| -> Vec<&Record> {
        let mut tasks: Vec<&Record> = records
            .iter()
            .filter(|r| r.kind == "Task")
            .filter(|r| match stage {
                Some(s) => stage_of(&by_id, r).is_some_and(|of| of.id == s.id),
                None => true,
            })
            .collect();
        tasks.sort_by_key(|t| t.order.unwrap_or(0));
        tasks
    };

    let waiting_approval = records
        .iter()
        .any(|r| r.kind == "Checkpoint.Approval" && is_state(r, "inProgress"));

    // Current stage: the one running, else the first one not finished.
    let current = stages
        .iter()
        .find(|s| is_state(s, "inProgress"))
        .or_else(|| stages.iter().find(|s| !is_state(s, "completed")))
        .copied();
    let tasks = tasks_of(current);
    let done_tasks = tasks.iter().filter(|t| is_state(t, "completed")).count();
    let position = tasks
        .iter()
        .position(|t| is_state(t, "inProgress"))
        .or_else(|| tasks.iter().position(|t| !is_state(t, "completed")));

    let fraction = if stages.is_empty() {
        (!tasks.is_empty()).then(|| done_tasks as f32 / tasks.len() as f32)
    } else {
        let done_stages = stages.iter().filter(|s| is_state(s, "completed")).count();
        let share = if tasks.is_empty() || current.is_none() {
            0.0
        } else {
            done_tasks as f32 / tasks.len() as f32
        };
        Some((done_stages as f32 + share) / stages.len() as f32)
    };

    // Red alert: the first failed task in run order, else a failed job or stage.
    let failing_step = stages
        .iter()
        .map(|s| Some(*s))
        .chain(stages.is_empty().then_some(None))
        .find_map(|stage| {
            tasks_of(stage)
                .into_iter()
                .find(|t| is_failed(t))
                .map(|t| label(stage, &t.name))
        })
        .or_else(|| {
            records
                .iter()
                .filter(|r| matches!(r.kind.as_str(), "Job" | "Stage") && is_failed(r))
                .min_by_key(|r| (r.kind != "Job", r.order.unwrap_or(0)))
                .map(|r| label(stage_of(&by_id, r), &r.name))
        });

    View {
        waiting_approval,
        step: position.map(|i| label(current, &tasks[i].name)),
        step_no: position.map(|i| i as u32 + 1),
        step_count: position.map(|_| tasks.len() as u32),
        fraction,
        failing_step,
        finished: !stages.is_empty() && stages.iter().all(|s| is_state(s, "completed")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::azure_devops::payload::Timeline;

    fn view(json: &str) -> View {
        analyse(&serde_json::from_str::<Timeline>(json).unwrap().records)
    }

    #[test]
    fn running_deploy_shows_stage_and_task() {
        let v = view(include_str!("testdata/timeline_running.json"));
        assert_eq!(v.step.as_deref(), Some("apply: terraform apply"));
        assert_eq!((v.step_no, v.step_count), (Some(4), Some(5)));
        // plan done, apply 3 of 5 tasks done: (1 + 0.6) / 2.
        assert!((v.fraction.unwrap() - 0.8).abs() < 1e-6);
        assert!(!v.waiting_approval && !v.finished);
        assert_eq!(v.failing_step, None);
    }

    #[test]
    fn waiting_for_approval() {
        let v = view(include_str!("testdata/timeline_approval.json"));
        assert!(v.waiting_approval);
        assert!((v.fraction.unwrap() - 0.5).abs() < 1e-6);
        assert!(v.step.is_none(), "the stage has no tasks yet");
    }

    #[test]
    fn failed_build_names_the_first_failed_task() {
        let v = view(include_str!("testdata/timeline_failed.json"));
        // The implicit stage is left out of the name.
        assert_eq!(v.failing_step.as_deref(), Some("Run unit tests"));
        assert!(v.finished);
        assert!(!v.waiting_approval);
    }

    #[test]
    fn empty_or_odd_timelines_do_not_panic() {
        assert_eq!(view(r#"{"records":[]}"#), View::default());
        // No stages: tasks count as one stage; a record pointing at a missing parent is ignored.
        let v = view(
            r#"{"records":[
                {"id":"a","parentId":"gone","type":"Task","name":"one","state":"completed","order":1},
                {"id":"b","parentId":null,"type":"Task","name":"two","state":"inProgress","order":2}]}"#,
        );
        assert_eq!(v.step.as_deref(), Some("two"));
        assert_eq!(v.fraction, Some(0.5));
        assert!(!v.finished);
    }

    #[test]
    fn a_failed_job_is_named_when_no_task_failed() {
        let v = view(
            r#"{"records":[
                {"id":"s","parentId":null,"type":"Stage","name":"apply","state":"completed","result":"failed","order":1},
                {"id":"j","parentId":"s","type":"Job","name":"Apply","state":"completed","result":"failed","order":1}]}"#,
        );
        assert_eq!(v.failing_step.as_deref(), Some("apply: Apply"));
    }
}
