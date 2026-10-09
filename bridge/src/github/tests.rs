//! GitHub source tests: the state machine fed with the sanitised fixtures,
//! and the whole source against a mock GitHub Enterprise Server.

use std::sync::Arc;

use serde_json::json;
use tokio::sync::mpsc;

use super::*;
use crate::ci::CiFacts;
use crate::model::{Level, RowStatus, ScreenData, pick};
use crate::source::{SourceBody, SourceMsg};

/// 2026-10-08T18:02:00Z, just after the fixtures' newest run started.
const NOW: u64 = 1_791_482_520;

fn fixture_pulls() -> Vec<PullRequest> {
    serde_json::from_str(include_str!("testdata/pulls.json")).unwrap()
}

fn fixture_runs() -> Vec<WorkflowRun> {
    let runs: payload::WorkflowRuns =
        serde_json::from_str(include_str!("testdata/runs.json")).unwrap();
    runs.workflow_runs
}

fn fixture_jobs() -> Vec<Job> {
    let jobs: payload::Jobs = serde_json::from_str(include_str!("testdata/jobs.json")).unwrap();
    jobs.jobs
}

fn state(interrupt: bool, since: u64) -> GithubState {
    GithubState::new(
        SourceId::new(KIND, "personal"),
        Interrupt::All(interrupt),
        BTreeMap::new(),
        true,
        since,
    )
}

fn apply(facts: &mut CiFacts, updates: Vec<Update>, now: u64) {
    for update in updates {
        facts.apply(update, now);
    }
}

fn top(facts: &CiFacts, now: u64) -> Option<(Level, ScreenData)> {
    let candidates = facts.candidates();
    pick(&candidates, now).map(|c| (c.level, c.data.clone()))
}

const REPO: &str = "your-user/demo";

#[test]
fn pulls_count_and_new_pr_flash() {
    let mut source = state(true, 0);
    let mut facts = CiFacts::default();

    // First poll: the PRs that are already open do not flash.
    apply(&mut facts, source.on_pulls(REPO, fixture_pulls(), NOW), NOW);
    assert_eq!(facts.open_pull_count(), 2);
    assert!(facts.candidates().is_empty());
    assert_eq!(facts.pulls_page().rows[0].sub, "demo #42");

    // A new PR appears: one flash. A new draft does not flash but counts.
    let mut pulls = fixture_pulls();
    pulls.push(
        serde_json::from_value(json!({ "number": 43, "title": "Fix login", "draft": false }))
            .unwrap(),
    );
    pulls.push(
        serde_json::from_value(json!({ "number": 44, "title": "Try things", "draft": true }))
            .unwrap(),
    );
    apply(&mut facts, source.on_pulls(REPO, pulls, NOW + 60), NOW + 60);
    assert_eq!(facts.open_pull_count(), 4);
    let Some((Level::Notice, ScreenData::Notice(notice))) = top(&facts, NOW + 60) else {
        panic!("expected a New PR notice");
    };
    assert_eq!(notice.sub, "demo #43: Fix login");
    assert_eq!(facts.candidates().len(), 1, "no flash for the draft");

    // Merged and closed PRs leave the count.
    apply(
        &mut facts,
        source.on_pulls(REPO, vec![], NOW + 120),
        NOW + 120,
    );
    assert_eq!(facts.open_pull_count(), 0);
}

#[test]
fn notify_new_prs_off() {
    let mut source = GithubState::new(
        SourceId::new(KIND, "work"),
        Interrupt::All(true),
        BTreeMap::new(),
        false,
        0,
    );
    source.on_pulls(REPO, vec![], NOW);
    let updates = source.on_pulls(REPO, fixture_pulls(), NOW + 60);
    assert!(
        updates
            .iter()
            .all(|u| !matches!(u, Update::PullOpened { .. }))
    );
}

#[test]
fn runs_fill_pipelines_page_with_latest_per_workflow() {
    let mut source = state(true, 0);
    let mut facts = CiFacts::default();
    apply(&mut facts, source.on_runs(REPO, fixture_runs(), NOW), NOW);

    let page = facts.pipelines_page();
    let rows: Vec<(&str, RowStatus)> = page
        .rows
        .iter()
        .map(|r| (r.text.as_str(), r.status))
        .collect();
    // ci.yml shows its newest run (in progress), not the older green one.
    assert_eq!(
        rows,
        [
            ("ci.yml", RowStatus::Running),
            ("deploy.yml", RowStatus::Failed),
            ("docs.yml", RowStatus::Neutral),
        ]
    );
    assert_eq!(page.rows[0].sub, "demo main");

    // Same answer again (a 304): nothing new to send.
    let again = source.on_runs(REPO, fixture_runs(), NOW + 60);
    assert!(
        again.iter().all(|u| !matches!(u, Update::Run { .. })),
        "{again:?}"
    );
}

#[test]
fn runs_finished_before_startup_stay_off_screen() {
    // The bridge started after the deploy failed: it counts in the badge but
    // does not take over the screen.
    let mut source = state(true, NOW);
    let mut facts = CiFacts::default();
    apply(&mut facts, source.on_runs(REPO, fixture_runs(), NOW), NOW);
    assert_eq!(facts.failed_count(), 1);
    assert!(
        facts
            .candidates()
            .iter()
            .all(|c| c.level != Level::AlertFailed)
    );
}

#[test]
fn fresh_failure_names_the_step() {
    let mut source = state(true, 0);
    let mut facts = CiFacts::default();
    let runs = fixture_runs();
    assert_eq!(source.need_failed_step(REPO, &runs), [1002]);
    source.set_failed_step(REPO, 1002, Some("terraform apply".into()));
    assert!(
        source.need_failed_step(REPO, &runs).is_empty(),
        "looked up once"
    );

    apply(&mut facts, source.on_runs(REPO, runs, NOW), NOW);
    let alert = facts
        .candidates()
        .into_iter()
        .find_map(|c| match c.data {
            ScreenData::Alert(a) if c.level == Level::AlertFailed => Some(a),
            _ => None,
        })
        .expect("red alert");
    assert_eq!(alert.pipeline.as_deref(), Some("deploy.yml"));
    assert_eq!(alert.step.as_deref(), Some("terraform apply"));
    assert_eq!(alert.source, "github");
    assert_eq!(alert.finished, Some(alert.started + 238));
}

#[test]
fn running_job_shows_progress_until_the_run_finishes() {
    let mut source = state(true, NOW);
    let mut facts = CiFacts::default();
    apply(&mut facts, source.on_runs(REPO, fixture_runs(), NOW), NOW);
    assert_eq!(source.watched_runs(), [(REPO.to_string(), 1003)]);

    let (updates, done) = source.on_jobs(REPO, 1003, fixture_jobs(), NOW + 5);
    assert!(!done);
    apply(&mut facts, updates, NOW + 5);
    let Some((Level::Job, ScreenData::Job(job))) = top(&facts, NOW + 5) else {
        panic!("expected a job screen");
    };
    assert_eq!(job.source, "github");
    assert_eq!(job.pipeline, "ci.yml");
    assert_eq!(job.commit, "a1b2c3d");
    assert_eq!(job.step.as_deref(), Some("cargo test"));
    assert_eq!((job.step_no, job.step_count), (Some(3), Some(4)));
    assert_eq!(job.progress, Some(0.5));

    // The job finishes: off screen, and the run result is worth fetching.
    let mut jobs = fixture_jobs();
    jobs[0].status = "completed".into();
    jobs[0].conclusion = Some("success".into());
    let (updates, done) = source.on_jobs(REPO, 1003, jobs, NOW + 60);
    assert!(done);
    apply(&mut facts, updates, NOW + 60);
    assert!(facts.candidates().is_empty());

    // The next runs poll sees the run completed: green flash, watch ends.
    let mut runs = fixture_runs();
    runs[0].status = Some("completed".into());
    runs[0].conclusion = Some("success".into());
    runs[0].updated_at = Some("2026-10-08T18:03:00Z".into());
    apply(&mut facts, source.on_runs(REPO, runs, NOW + 61), NOW + 61);
    assert_eq!(top(&facts, NOW + 61).unwrap().0, Level::AlertSuccess);
    assert!(source.watched_runs().is_empty());
    assert_eq!(source.take_finished_runs(), [(REPO.to_string(), 1003)]);

    // A late jobs answer for the finished run changes nothing.
    let (updates, _) = source.on_jobs(REPO, 1003, fixture_jobs(), NOW + 62);
    assert!(updates.is_empty());
}

#[test]
fn run_finishing_between_polls_clears_its_jobs() {
    let mut source = state(true, NOW);
    let mut facts = CiFacts::default();
    apply(&mut facts, source.on_runs(REPO, fixture_runs(), NOW), NOW);
    let (updates, _) = source.on_jobs(REPO, 1003, fixture_jobs(), NOW + 5);
    apply(&mut facts, updates, NOW + 5);
    assert!(facts.has_job("github:personal:your-user/demo:job:2001"));

    // The run failed before the next jobs poll.
    let mut runs = fixture_runs();
    runs[0].status = Some("completed".into());
    runs[0].conclusion = Some("failure".into());
    runs[0].updated_at = Some("2026-10-08T18:03:00Z".into());
    apply(&mut facts, source.on_runs(REPO, runs, NOW + 60), NOW + 60);
    assert!(!facts.has_job("github:personal:your-user/demo:job:2001"));
    assert_eq!(top(&facts, NOW + 60).unwrap().0, Level::AlertFailed);
}

#[test]
fn quiet_repos_are_job_polled_but_stay_off_the_esp_screen() {
    let mut source = state(false, 0);
    let mut facts = CiFacts::default();
    apply(&mut facts, source.on_runs(REPO, fixture_runs(), NOW), NOW);
    assert_eq!(facts.pipelines_page().count, 3);
    assert_eq!(source.watched_runs(), [(REPO.to_string(), 1003)]);

    let (updates, _) = source.on_jobs(REPO, 1003, fixture_jobs(), NOW + 5);
    apply(&mut facts, updates, NOW + 5);
    assert!(facts.has_job("github:personal:your-user/demo:job:2001"));
    assert_eq!(facts.jobs().len(), 1, "the kiosk list sees it");
    assert!(facts.candidates().is_empty(), "the ESP screen does not");
}

#[test]
fn health_worst_repo_wins() {
    let mut source = state(true, 0);
    assert_eq!(source.health(), None);
    source.set_health("me/a", Health::Ok);
    source.set_health("me/b", Health::AuthFailed);
    assert_eq!(source.health(), Some(Health::AuthFailed));
    source.set_health("me/b", Health::Ok);
    assert_eq!(source.health(), Some(Health::Ok));
}

#[test]
fn status_mapping() {
    let cases = [
        ("queued", None, RunStatus::Queued),
        ("waiting", None, RunStatus::Queued),
        ("pending", None, RunStatus::Queued),
        ("in_progress", None, RunStatus::Running),
        ("completed", Some("success"), RunStatus::Success),
        ("completed", Some("failure"), RunStatus::Failed),
        ("completed", Some("timed_out"), RunStatus::Failed),
        ("completed", Some("startup_failure"), RunStatus::Failed),
        ("completed", Some("cancelled"), RunStatus::Neutral),
        ("completed", Some("skipped"), RunStatus::Neutral),
        ("completed", Some("neutral"), RunStatus::Neutral),
    ];
    for (status, conclusion, expected) in cases {
        assert_eq!(
            job_status(status, conclusion),
            expected,
            "{status} {conclusion:?}"
        );
    }
}

#[test]
fn backoff_and_quota_slowdown() {
    let poll = Duration::from_secs(60);
    assert_eq!(next_delay(poll, 0, false), poll);
    assert_eq!(next_delay(poll, 2, false), Duration::from_secs(120));
    assert_eq!(next_delay(poll, 9, false), Duration::from_secs(300));
    assert_eq!(next_delay(poll, 0, true), Duration::from_secs(240));
}

#[tokio::test]
async fn source_polls_a_mock_enterprise_server() {
    let seen = Arc::new(api::tests::Seen::default());
    let base_url = api::tests::mock_github(seen.clone()).await;
    let config: GithubConfig = toml::from_str(&format!(
        "name = \"work-ghe\"\nbase_url = \"{base_url}\"\ntoken_file = \"unused\"\n\
         repos = [\"{REPO}\"]\njob_poll_s = 1\nalias = {{ \"{REPO}\" = \"Demo\" }}"
    ))
    .unwrap();
    let source = Github {
        id: SourceId::new(KIND, &config.name),
        api: HttpApi::new(&config.base_url, crate::source::Secret::new("t")).unwrap(),
        config,
    };
    let (tx, mut rx) = mpsc::channel(64);
    crate::source::spawn(source, tx);

    let mut health = None;
    let (mut pulls, mut runs, mut job) = (false, false, None);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !(pulls && runs && job.is_some() && health.is_some()) {
        let msg: SourceMsg = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .expect("source reported everything in time")
            .unwrap();
        assert_eq!(msg.source.to_string(), "github:work-ghe");
        match msg.body {
            SourceBody::Health(h) => health = Some(h),
            SourceBody::Facts(updates) => {
                for update in updates {
                    match update {
                        Update::Pulls { repo, .. } => {
                            assert_eq!((repo.project.as_str(), repo.total), ("Demo", 2));
                            pulls = true;
                        }
                        Update::Run { .. } => runs = true,
                        Update::JobRunning { job: j, .. } => job = Some(j),
                        _ => {}
                    }
                }
            }
            SourceBody::Stats(_) => panic!("GitHub sends no host stats"),
        }
    }
    assert_eq!(health, Some(Health::Ok));
    assert_eq!(job.unwrap().data.step.as_deref(), Some("cargo test"));

    // The failed deploy run's jobs were fetched once for its failing step.
    let requests = seen.requests.lock().unwrap();
    assert!(requests.iter().any(|(path, _)| path == "jobs"));
}
