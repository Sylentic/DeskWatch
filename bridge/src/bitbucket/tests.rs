//! Bitbucket source tests: the state machine fed with fixtures written from
//! the published API description, and the whole source against a mock server.

use std::sync::Arc;

use serde_json::json;
use tokio::sync::mpsc;

use super::*;
use crate::ci::{CiFacts, RunnerStatus};
use crate::model::{Level, RowStatus, ScreenData, pick};
use crate::source::{SourceBody, SourceMsg};
use payload::Page;

/// 2026-10-08T18:02:00Z, just after the fixtures' newest pipeline started.
const NOW: u64 = 1_791_482_520;

const REPO: &str = "your-workspace/demo";
const JOB_KEY: &str = "bitbucket:personal:your-workspace/demo:pipeline:14";
const FAILED_UUID: &str = "{aaaa0003-0000-4000-8000-000000000003}";

fn fixture_prs() -> Vec<PullRequest> {
    let page: Page<PullRequest> =
        serde_json::from_str(include_str!("testdata/pullrequests.json")).unwrap();
    page.values
}

fn fixture_pipelines() -> Vec<Pipeline> {
    let page: Page<Pipeline> =
        serde_json::from_str(include_str!("testdata/pipelines.json")).unwrap();
    page.values
}

fn fixture_steps() -> Vec<Step> {
    let page: Page<Step> = serde_json::from_str(include_str!("testdata/steps.json")).unwrap();
    page.values
}

fn state(interrupt: bool, since: u64) -> BitbucketState {
    BitbucketState::new(
        SourceId::new(KIND, "personal"),
        Interrupt::All(interrupt),
        BTreeMap::new(),
        true,
        since,
    )
    .with_api_base("https://api.bitbucket.org/2.0")
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

#[test]
fn pull_requests_count_with_a_new_pr_flash() {
    let mut source = state(true, 0);
    let mut facts = CiFacts::default();

    // First poll: the PRs that are already open do not flash.
    apply(
        &mut facts,
        source.on_pulls(REPO, fixture_prs(), None, NOW),
        NOW,
    );
    assert_eq!(facts.open_pull_count(), 2);
    assert!(facts.candidates().is_empty());
    assert_eq!(facts.pulls_page().rows[0].sub, "demo #43");

    // A new PR appears: one flash. A new draft does not flash but counts.
    let mut prs = fixture_prs();
    prs.push(serde_json::from_value(json!({ "id": 44, "title": "Fix login" })).unwrap());
    prs.push(serde_json::from_value(json!({ "id": 45, "title": "Try", "draft": true })).unwrap());
    apply(
        &mut facts,
        source.on_pulls(REPO, prs, None, NOW + 60),
        NOW + 60,
    );
    assert_eq!(facts.open_pull_count(), 4);
    let Some((Level::Notice, ScreenData::Notice(notice))) = top(&facts, NOW + 60) else {
        panic!("expected a New PR notice");
    };
    assert_eq!(notice.sub, "demo #44: Fix login");
    assert_eq!(facts.candidates().len(), 1, "no flash for the draft");

    // Merged and declined PRs leave the count.
    apply(
        &mut facts,
        source.on_pulls(REPO, vec![], None, NOW + 120),
        NOW + 120,
    );
    assert_eq!(facts.open_pull_count(), 0);
}

#[test]
fn the_exact_total_beats_the_pages_read() {
    let mut source = state(true, 0);
    let mut facts = CiFacts::default();
    apply(
        &mut facts,
        source.on_pulls(REPO, fixture_prs(), Some(300), NOW),
        NOW,
    );
    assert_eq!(facts.open_pull_count(), 300);
    apply(
        &mut facts,
        source.on_pulls(REPO, fixture_prs(), Some(1), NOW + 60),
        NOW + 60,
    );
    assert_eq!(facts.open_pull_count(), 2);
}

#[test]
fn notify_new_prs_off() {
    let mut source = BitbucketState::new(
        SourceId::new(KIND, "work"),
        Interrupt::All(true),
        BTreeMap::new(),
        false,
        0,
    );
    source.on_pulls(REPO, vec![], None, NOW);
    let updates = source.on_pulls(REPO, fixture_prs(), None, NOW + 60);
    assert!(
        updates
            .iter()
            .all(|u| !matches!(u, Update::PullOpened { .. }))
    );
}

#[test]
fn pipelines_fill_the_pipelines_page_with_the_latest_per_name() {
    let mut source = state(true, 0);
    let mut facts = CiFacts::default();
    apply(
        &mut facts,
        source.on_runs(REPO, fixture_pipelines(), NOW),
        NOW,
    );

    let page = facts.pipelines_page();
    let rows: Vec<(&str, RowStatus)> = page
        .rows
        .iter()
        .map(|r| (r.text.as_str(), r.status))
        .collect();
    // The default pipeline shows its newest run (in progress), the older
    // stopped one is gone. `deploy-prod` shows its failure, not the older green.
    assert_eq!(
        rows,
        [
            ("bitbucket-pipelines.yml", RowStatus::Running),
            ("deploy-prod", RowStatus::Failed),
        ]
    );
    assert_eq!(page.rows[0].sub, "demo main");

    // Same answer again: nothing new to send.
    let again = source.on_runs(REPO, fixture_pipelines(), NOW + 60);
    assert!(
        again.iter().all(|u| !matches!(u, Update::Run { .. })),
        "{again:?}"
    );
}

#[test]
fn pipelines_finished_before_startup_stay_off_screen() {
    let mut source = state(true, NOW);
    let mut facts = CiFacts::default();
    apply(
        &mut facts,
        source.on_runs(REPO, fixture_pipelines(), NOW),
        NOW,
    );
    assert_eq!(facts.failed_count(), 1);
    assert!(
        facts
            .candidates()
            .iter()
            .all(|c| c.level != Level::AlertFailed)
    );
}

#[test]
fn fresh_failure_names_the_failing_step() {
    let mut source = state(true, 0);
    let mut facts = CiFacts::default();
    let pipelines = fixture_pipelines();
    assert_eq!(
        source.need_failed_step(REPO, &pipelines),
        [(13, FAILED_UUID.to_string())]
    );
    let failed: Page<Step> =
        serde_json::from_str(include_str!("testdata/failed_steps.json")).unwrap();
    source.set_failed_step(REPO, 13, failing_step(&failed.values));
    assert!(
        source.need_failed_step(REPO, &pipelines).is_empty(),
        "looked up once"
    );

    apply(&mut facts, source.on_runs(REPO, pipelines, NOW), NOW);
    let alert = facts
        .candidates()
        .into_iter()
        .find_map(|c| match c.data {
            ScreenData::Alert(a) if c.level == Level::AlertFailed => Some(a),
            _ => None,
        })
        .expect("red alert");
    assert_eq!(alert.pipeline.as_deref(), Some("deploy-prod"));
    assert_eq!(alert.step.as_deref(), Some("Unit tests"));
    assert_eq!(alert.source, "bitbucket");
    assert_eq!(alert.finished, Some(alert.started + 180));
}

#[test]
fn running_pipeline_shows_step_progress_until_it_finishes() {
    let mut source = state(true, NOW);
    let mut facts = CiFacts::default();
    apply(
        &mut facts,
        source.on_runs(REPO, fixture_pipelines(), NOW),
        NOW,
    );
    assert_eq!(
        source.watched_pipelines(),
        [(
            REPO.to_string(),
            14,
            "{aaaa0004-0000-4000-8000-000000000004}".to_string()
        )]
    );

    let (updates, done) = source.on_steps(REPO, 14, fixture_steps(), NOW + 5);
    assert!(!done);
    apply(&mut facts, updates, NOW + 5);
    let Some((Level::Job, ScreenData::Job(job))) = top(&facts, NOW + 5) else {
        panic!("expected a job screen");
    };
    assert_eq!(job.source, "bitbucket");
    assert_eq!(job.pipeline, "bitbucket-pipelines.yml");
    assert_eq!(job.git_ref, "main");
    assert_eq!(job.commit, "a1b2c3d");
    assert_eq!(job.step.as_deref(), Some("Integration tests"));
    assert_eq!((job.step_no, job.step_count), (Some(3), Some(4)));
    assert_eq!(job.progress, Some(0.5));
    assert_eq!(
        job.started,
        unix_seconds(Some("2026-10-08T18:00:35+00:00")).unwrap()
    );

    // Every step ends: the pipeline result is worth fetching now.
    let mut steps = fixture_steps();
    for step in &mut steps {
        step.state.name = "COMPLETED".into();
    }
    let (updates, done) = source.on_steps(REPO, 14, steps, NOW + 60);
    assert!(done);
    apply(&mut facts, updates, NOW + 60);

    // The next pipelines poll sees it succeeded: green flash, card gone.
    let mut pipelines = fixture_pipelines();
    pipelines[0].state = serde_json::from_value(json!({
        "name": "COMPLETED", "result": { "name": "SUCCESSFUL" }
    }))
    .unwrap();
    pipelines[0].completed_on = Some("2026-10-08T18:03:00.000000+00:00".into());
    apply(
        &mut facts,
        source.on_runs(REPO, pipelines, NOW + 61),
        NOW + 61,
    );
    assert_eq!(top(&facts, NOW + 61).unwrap().0, Level::AlertSuccess);
    assert!(!facts.has_job(JOB_KEY));
    assert!(source.watched_pipelines().is_empty());

    // A late steps answer for the finished pipeline changes nothing.
    let (updates, _) = source.on_steps(REPO, 14, fixture_steps(), NOW + 62);
    assert!(updates.is_empty());
}

#[test]
fn pipeline_finishing_between_polls_clears_its_card() {
    let mut source = state(true, NOW);
    let mut facts = CiFacts::default();
    apply(
        &mut facts,
        source.on_runs(REPO, fixture_pipelines(), NOW),
        NOW,
    );
    let (updates, _) = source.on_steps(REPO, 14, fixture_steps(), NOW + 5);
    apply(&mut facts, updates, NOW + 5);
    assert!(facts.has_job(JOB_KEY));

    // The pipeline failed before the next steps poll.
    let mut pipelines = fixture_pipelines();
    pipelines[0].state = serde_json::from_value(json!({
        "name": "COMPLETED", "result": { "name": "FAILED" }
    }))
    .unwrap();
    pipelines[0].completed_on = Some("2026-10-08T18:03:00.000000+00:00".into());
    apply(
        &mut facts,
        source.on_runs(REPO, pipelines, NOW + 60),
        NOW + 60,
    );
    assert!(!facts.has_job(JOB_KEY));
    assert_eq!(top(&facts, NOW + 60).unwrap().0, Level::AlertFailed);
}

#[test]
fn quiet_repos_are_step_polled_but_stay_off_the_esp_screen() {
    let mut source = state(false, 0);
    let mut facts = CiFacts::default();
    apply(
        &mut facts,
        source.on_runs(REPO, fixture_pipelines(), NOW),
        NOW,
    );
    let (updates, _) = source.on_steps(REPO, 14, fixture_steps(), NOW + 5);
    apply(&mut facts, updates, NOW + 5);
    assert!(facts.has_job(JOB_KEY));
    assert_eq!(facts.jobs().len(), 1, "the kiosk list sees it");
    assert!(facts.candidates().is_empty(), "the ESP screen does not");
}

#[test]
fn step_progress_without_a_running_step_or_a_name() {
    // Nothing in progress (waiting for a runner): the first unfinished step.
    let mut steps = fixture_steps();
    steps[2].state.name = "READY".into();
    let progress = step_progress(&steps);
    assert_eq!(progress.step.as_deref(), Some("Integration tests"));
    assert_eq!(progress.step_no, Some(3));

    // A step without a name reads "step N".
    steps[2].name = None;
    assert_eq!(step_progress(&steps).step.as_deref(), Some("step 3"));

    assert_eq!(step_progress(&[]), StepProgress::default());
}

#[test]
fn custom_deploy_pipelines_are_deploys() {
    let mut source = state(true, NOW);
    let mut pipelines = fixture_pipelines();
    pipelines[0].target.selector = Some(payload::Selector {
        kind: Some("custom".into()),
        pattern: Some("deploy-prod".into()),
    });
    source.on_runs(REPO, pipelines, NOW);
    let (updates, _) = source.on_steps(REPO, 14, fixture_steps(), NOW + 5);
    let kind = updates.iter().find_map(|u| match u {
        Update::JobRunning { job, .. } => Some(job.data.kind),
        _ => None,
    });
    assert_eq!(kind, Some(crate::model::JobKind::Deploy));
}

#[test]
fn health_worst_repo_wins() {
    let mut source = state(true, 0);
    assert_eq!(source.health(), None);
    source.set_health("w/a", Health::Ok);
    source.set_health("w/b", Health::AuthFailed);
    assert_eq!(source.health(), Some(Health::AuthFailed));
    source.set_health("w/b", Health::Ok);
    assert_eq!(source.health(), Some(Health::Ok));
}

#[test]
fn status_mapping() {
    let cases = [
        ("PENDING", None, None, RunStatus::Queued),
        ("IN_PROGRESS", Some("RUNNING"), None, RunStatus::Running),
        ("IN_PROGRESS", None, None, RunStatus::Running),
        ("IN_PROGRESS", Some("PAUSED"), None, RunStatus::Waiting),
        ("COMPLETED", None, Some("SUCCESSFUL"), RunStatus::Success),
        ("COMPLETED", None, Some("FAILED"), RunStatus::Failed),
        ("COMPLETED", None, Some("ERROR"), RunStatus::Failed),
        ("COMPLETED", None, Some("STOPPED"), RunStatus::Neutral),
        ("COMPLETED", None, Some("EXPIRED"), RunStatus::Neutral),
        ("COMPLETED", None, None, RunStatus::Neutral),
        ("SOMETHING_NEW", None, None, RunStatus::Neutral),
    ];
    for (state, stage, result, expected) in cases {
        assert_eq!(
            pipeline_status(state, stage, result),
            expected,
            "{state} {stage:?} {result:?}"
        );
    }
}

#[test]
fn backoff() {
    let poll = Duration::from_secs(120);
    assert_eq!(next_delay(poll, 0), poll);
    assert_eq!(next_delay(poll, 2), Duration::from_secs(180));
    assert_eq!(next_delay(poll, 9), Duration::from_secs(300));
}

#[test]
fn links_come_from_bitbucket_and_bad_ones_are_dropped() {
    let mut source = state(true, 0);
    let prs: Vec<PullRequest> = serde_json::from_value(json!([
        { "id": 5, "title": "ok", "links": { "html": { "href": "https://bitbucket.org/w/r/pull-requests/5" } } },
        { "id": 6, "title": "odd", "links": { "html": { "href": "javascript:alert(1)" } } },
        { "id": 7, "title": "none" }
    ]))
    .unwrap();
    let updates = source.on_pulls(REPO, prs, None, NOW);
    let Some(Update::Pulls { repo, .. }) = updates.last() else {
        panic!("expected the PR list");
    };
    let urls: Vec<_> = repo.pulls.iter().map(|p| p.url.as_deref()).collect();
    assert_eq!(
        urls,
        [
            Some("https://bitbucket.org/w/r/pull-requests/5"),
            None,
            None
        ]
    );

    // A run and its card link to the pipeline page, built from the build number.
    let updates = source.on_runs(REPO, fixture_pipelines(), NOW);
    let run_url = updates.iter().find_map(|u| match u {
        Update::Run { run, .. } => run.url.clone(),
        _ => None,
    });
    let expected = "https://bitbucket.org/your-workspace/demo/pipelines/results/14";
    assert_eq!(run_url.as_deref(), Some(expected));
    let (updates, _) = source.on_steps(REPO, 14, fixture_steps(), NOW + 5);
    let job_url = updates.iter().find_map(|u| match u {
        Update::JobRunning { job, .. } => job.url.clone(),
        _ => None,
    });
    assert_eq!(job_url.as_deref(), Some(expected));

    // An API address we cannot map makes no link at all.
    let other = state(true, 0).with_api_base("http://127.0.0.1:9999/2.0");
    assert_eq!(other.pipeline_url(REPO, 14), None);
}

fn source_for(config: &str) -> Bitbucket {
    let config: BitbucketConfig = toml::from_str(config).unwrap();
    Bitbucket {
        id: SourceId::new(KIND, &config.name),
        api: HttpApi::new(&config.base_url, crate::source::Secret::new("t")).unwrap(),
        config,
    }
}

#[tokio::test]
async fn source_polls_a_mock_bitbucket() {
    let seen = Arc::new(api::tests::Seen::default());
    let base_url = api::tests::mock_bitbucket(seen.clone()).await;
    let source = source_for(&format!(
        "name = \"work\"\nbase_url = \"{base_url}\"\ntoken_file = \"unused\"\n\
         repos = [\"{REPO}\"]\njob_poll_s = 1\nalias = {{ \"{REPO}\" = \"Demo\" }}"
    ));
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
        assert_eq!(msg.source.to_string(), "bitbucket:work");
        match msg.body {
            SourceBody::Health(h) => health = Some(h),
            SourceBody::Forget => panic!("a source never forgets itself"),
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
            SourceBody::Stats(_) => panic!("Bitbucket sends no host stats"),
        }
    }
    assert_eq!(health, Some(Health::Ok));
    assert_eq!(job.unwrap().data.step.as_deref(), Some("Integration tests"));

    // The failed pipeline's steps were fetched once for its failing step.
    let requests = seen.requests.lock().unwrap();
    assert!(
        requests
            .iter()
            .any(|(what, _)| what == &format!("steps {FAILED_UUID}"))
    );
}

#[tokio::test]
async fn source_polls_runners_of_a_repository_and_a_workspace() {
    let seen = Arc::new(api::tests::Seen::default());
    let base_url = api::tests::mock_bitbucket(seen).await;
    let source = source_for(&format!(
        "name = \"work\"\nbase_url = \"{base_url}\"\ntoken_file = \"unused\"\n\
         repos = [\"{REPO}\"]\nrunners = [\"repo:{REPO}\", \"workspace:your-workspace\"]\n\
         runner_poll_s = 1"
    ));
    let (tx, mut rx) = mpsc::channel(64);
    crate::source::spawn(source, tx);

    let mut sets = BTreeMap::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while sets.len() < 2 {
        let msg = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .expect("runners reported in time")
            .unwrap();
        if let SourceBody::Facts(updates) = msg.body {
            for update in updates {
                if let Update::Runners { key, runners } = update {
                    sets.insert(key, runners);
                }
            }
        }
    }
    let keys: Vec<_> = sets.keys().cloned().collect();
    assert_eq!(
        keys,
        [
            format!("bitbucket:work:runners:repo:{REPO}"),
            "bitbucket:work:runners:workspace:your-workspace".to_string(),
        ]
    );
    let states: Vec<_> = sets[&keys[0]]
        .iter()
        .map(|r| (r.name.as_str(), r.status))
        .collect();
    assert_eq!(
        states,
        [
            ("build-box-1", RunnerStatus::Idle),
            ("build-box-2", RunnerStatus::Offline),
            ("build-box-3", RunnerStatus::Offline),
            ("retired-box", RunnerStatus::Offline),
        ]
    );
}

/// A token without the runner right gets 403 on the runner list only. That
/// must not make the source look broken: the PR and pipeline widgets stay fresh.
#[tokio::test]
async fn refused_runner_list_does_not_hurt_source_health() {
    use axum::Router;
    use axum::http::StatusCode;
    use axum::routing::get;

    let repo = "/2.0/repositories/{w}/{s}";
    let app = Router::new()
        .route(
            &format!("{repo}/pullrequests"),
            get(|| async { "{\"values\":[]}" }),
        )
        .route(
            &format!("{repo}/pipelines"),
            get(|| async { "{\"values\":[]}" }),
        )
        .route(
            &format!("{repo}/pipelines-config/runners"),
            get(|| async { (StatusCode::FORBIDDEN, "{\"type\":\"error\"}") }),
        );
    let base_url = format!("{}/2.0", api::tests::serve(app).await);
    let source = source_for(&format!(
        "name = \"home\"\nbase_url = \"{base_url}\"\ntoken_file = \"unused\"\n\
         repos = [\"{REPO}\"]\nrunners = [\"repo:{REPO}\"]\nrunner_poll_s = 1"
    ));
    let (tx, mut rx) = mpsc::channel(64);
    crate::source::spawn(source, tx);

    // Let several runner polls fail, then collect what arrived.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let mut healths = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        match msg.body {
            SourceBody::Health(h) => healths.push(h),
            SourceBody::Facts(updates) => {
                assert!(!updates.iter().any(|u| matches!(u, Update::Runners { .. })));
            }
            SourceBody::Stats(_) => panic!("Bitbucket sends no host stats"),
            SourceBody::Forget => panic!("a source never forgets itself"),
        }
    }
    assert_eq!(healths, [Health::Ok]);
}

#[test]
fn new_issues_raise_alerts_after_the_first_poll() {
    let issue = |id: u64, title: &str| -> Issue {
        serde_json::from_value(json!({
            "id": id,
            "title": title,
            "links": { "html": { "href": format!("https://bitbucket.org/team/app/issues/{id}") } },
            "reporter": { "display_name": "Dana" },
        }))
        .unwrap()
    };
    let mut state = state(true, 0).with_issue_alerts(true);
    assert!(
        state
            .on_issues("team/app", vec![issue(3, "old")])
            .is_empty()
    );
    let updates = state.on_issues("team/app", vec![issue(4, "broken build"), issue(3, "old")]);
    let [Update::Alert { alert }] = updates.as_slice() else {
        panic!("one alert expected, got {updates:?}");
    };
    assert_eq!(
        alert.message.as_deref(),
        Some("New issue #4 in app by Dana")
    );
    assert_eq!(alert.source.as_deref(), Some("bitbucket"));
    assert_eq!(
        alert.url.as_deref(),
        Some("https://bitbucket.org/team/app/issues/4")
    );
}

#[test]
fn every_step_of_the_pipeline_is_listed() {
    let mut source = state(false, 0);
    let mut facts = CiFacts::default();
    apply(
        &mut facts,
        source.on_runs(REPO, fixture_pipelines(), NOW),
        NOW,
    );
    let (updates, _) = source.on_steps(REPO, 14, fixture_steps(), NOW + 5);
    apply(&mut facts, updates, NOW + 5);
    let list = facts.run_jobs(JOB_KEY).expect("the pipeline's steps");
    let rows: Vec<_> = list.iter().map(|j| (j.name.as_str(), j.status)).collect();
    assert_eq!(
        rows,
        [
            ("Build", RunStatus::Success),
            ("Unit tests", RunStatus::Success),
            ("Integration tests", RunStatus::Running),
            ("Deploy", RunStatus::Queued),
        ]
    );
    assert_eq!(facts.jobs()[0].1.group.as_deref(), Some(JOB_KEY));
}
