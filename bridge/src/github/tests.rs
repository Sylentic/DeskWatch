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
    assert_eq!(facts.pull_rows().rows[0].sub, "demo #42");

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

    let page = facts.pipeline_rows();
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
fn quiet_repos_are_job_polled_but_stay_off_the_banner() {
    let mut source = state(false, 0);
    let mut facts = CiFacts::default();
    apply(&mut facts, source.on_runs(REPO, fixture_runs(), NOW), NOW);
    assert_eq!(facts.pipeline_rows().count, 3);
    assert_eq!(source.watched_runs(), [(REPO.to_string(), 1003)]);

    let (updates, _) = source.on_jobs(REPO, 1003, fixture_jobs(), NOW + 5);
    apply(&mut facts, updates, NOW + 5);
    assert!(facts.has_job("github:personal:your-user/demo:job:2001"));
    assert_eq!(facts.jobs().len(), 1, "the kiosk list sees it");
    assert!(facts.candidates().is_empty(), "the banner does not");
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
            SourceBody::Stats(_) => panic!("GitHub sends no host stats"),
        }
    }
    assert_eq!(health, Some(Health::Ok));
    assert_eq!(job.unwrap().data.step.as_deref(), Some("cargo test"));

    // The failed deploy run's jobs were fetched once for its failing step.
    let requests = seen.requests.lock().unwrap();
    assert!(requests.iter().any(|(path, _)| path == "jobs"));
}

fn source_for(config: &str) -> Github {
    let config: GithubConfig = toml::from_str(config).unwrap();
    Github {
        id: SourceId::new(KIND, &config.name),
        api: HttpApi::new(&config.base_url, crate::source::Secret::new("t")).unwrap(),
        config,
    }
}

#[tokio::test]
async fn source_polls_runners_of_a_repository_and_an_organisation() {
    let seen = Arc::new(api::tests::Seen::default());
    let base_url = api::tests::mock_github(seen).await;
    let source = source_for(&format!(
        "name = \"work-ghe\"\nbase_url = \"{base_url}\"\ntoken_file = \"unused\"\n\
         repos = [\"{REPO}\"]\nrunners = [\"repo:{REPO}\", \"org:your-org\"]\nrunner_poll_s = 1"
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
            "github:work-ghe:runners:org:your-org".to_string(),
            format!("github:work-ghe:runners:repo:{REPO}"),
        ]
    );
    let runners = &sets[&keys[0]];
    let states: Vec<_> = runners
        .iter()
        .map(|r| (r.name.as_str(), r.status))
        .collect();
    assert_eq!(
        states,
        [
            ("build-box-1", crate::ci::RunnerStatus::Idle),
            ("build-box-2", crate::ci::RunnerStatus::Busy),
            ("build-box-3", crate::ci::RunnerStatus::Offline),
        ]
    );
    assert_eq!(runners[1].labels, ["self-hosted", "gpu"]);
    assert_eq!(runners[0].source, "github");
}

/// A token without the runner right gets 403 on the runner list only. That
/// must not make the source look broken: the PR and pipeline widgets stay fresh.
#[tokio::test]
async fn refused_runner_list_does_not_hurt_source_health() {
    use axum::Router;
    use axum::http::StatusCode;
    use axum::routing::get;

    let repo = "/api/v3/repos/{owner}/{repo}";
    let app = Router::new()
        .route(&format!("{repo}/pulls"), get(|| async { "[]" }))
        .route(
            &format!("{repo}/actions/runs"),
            get(|| async { "{\"workflow_runs\":[]}" }),
        )
        .route(
            &format!("{repo}/actions/runners"),
            get(|| async {
                (
                    StatusCode::FORBIDDEN,
                    "{\"message\":\"Resource not accessible\"}",
                )
            }),
        );
    let base_url = api::tests::serve(app).await;
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
            SourceBody::Stats(_) => panic!("GitHub sends no host stats"),
            SourceBody::Forget => panic!("a source never forgets itself"),
        }
    }
    assert_eq!(healths, [Health::Ok]);
}

#[test]
fn kiosk_links_come_from_github_and_bad_ones_are_dropped() {
    let mut source = state(true, 0);
    let pulls: Vec<PullRequest> = serde_json::from_value(json!([
        { "number": 5, "title": "ok", "html_url": "https://github.com/your-user/demo/pull/5" },
        { "number": 6, "title": "odd", "html_url": "javascript:alert(1)" },
        { "number": 7, "title": "none" }
    ]))
    .unwrap();
    let updates = source.on_pulls(REPO, pulls, NOW);
    let Some(Update::Pulls { repo, .. }) = updates.last() else {
        panic!("expected the PR list");
    };
    let urls: Vec<_> = repo.pulls.iter().map(|p| p.url.as_deref()).collect();
    assert_eq!(
        urls,
        [Some("https://github.com/your-user/demo/pull/5"), None, None]
    );

    // A run links to itself, and its job to the job page or else the run.
    let runs: Vec<WorkflowRun> = serde_json::from_value(json!([{
        "id": 9, "path": ".github/workflows/ci.yml", "status": "in_progress",
        "head_branch": "main", "html_url": "https://github.com/your-user/demo/actions/runs/9"
    }]))
    .unwrap();
    let updates = source.on_runs(REPO, runs, NOW);
    let run_url = updates.iter().find_map(|u| match u {
        Update::Run { run, .. } => run.url.clone(),
        _ => None,
    });
    assert_eq!(
        run_url.as_deref(),
        Some("https://github.com/your-user/demo/actions/runs/9")
    );
    let jobs: Vec<Job> = serde_json::from_value(json!([
        { "id": 1, "run_id": 9, "name": "a", "status": "in_progress",
          "html_url": "https://github.com/your-user/demo/actions/runs/9/job/1" },
        { "id": 2, "run_id": 9, "name": "b", "status": "in_progress" }
    ]))
    .unwrap();
    let (updates, _) = source.on_jobs(REPO, 9, jobs, NOW + 5);
    let urls: Vec<_> = updates
        .iter()
        .filter_map(|u| match u {
            Update::JobRunning { job, .. } => job.url.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(
        urls,
        [
            "https://github.com/your-user/demo/actions/runs/9/job/1",
            "https://github.com/your-user/demo/actions/runs/9"
        ]
    );
}

#[test]
fn runner_pages_follow_the_api_address() {
    let links = |api: &str| {
        let source = state(true, 0).with_api_base(api);
        let scope = |text: &str| source.runners_url(&RunnerScope::parse(text).unwrap());
        (scope("repo:me/demo"), scope("org:team"), scope("admin"))
    };
    let (repo, org, admin) = links("https://api.github.com");
    assert_eq!(
        repo.as_deref(),
        Some("https://github.com/me/demo/settings/actions/runners")
    );
    assert_eq!(
        org.as_deref(),
        Some("https://github.com/organizations/team/settings/actions/runners")
    );
    assert_eq!(admin, None, "no such page on GitHub");
    // Enterprise Server and GHE.com.
    assert_eq!(
        links("https://ghe.example/api/v3").0.as_deref(),
        Some("https://ghe.example/me/demo/settings/actions/runners")
    );
    assert_eq!(
        links("https://api.acme.ghe.com").0.as_deref(),
        Some("https://acme.ghe.com/me/demo/settings/actions/runners")
    );
    // An address we cannot map makes no link at all.
    assert_eq!(links("http://127.0.0.1:9999").0, None);
}

fn issue(number: u64, title: &str, pull: bool) -> Issue {
    serde_json::from_value(json!({
        "number": number,
        "title": title,
        "html_url": format!("https://github.com/me/app/issues/{number}"),
        "user": { "login": "dana" },
        "pull_request": if pull { json!({}) } else { json!(null) },
    }))
    .unwrap()
}

#[test]
fn new_issues_raise_alerts_after_the_first_poll() {
    let mut state = state(true, 0).with_issue_alerts(true);
    // First poll: what is open already is old news.
    assert!(
        state
            .on_issues("me/app", vec![issue(3, "old bug", false)])
            .is_empty()
    );
    // A new issue and a new PR (same number space): only the issue alerts.
    let updates = state.on_issues(
        "me/app",
        vec![
            issue(5, "crash on start", false),
            issue(4, "a PR", true),
            issue(3, "old bug", false),
        ],
    );
    let [Update::Alert { alert }] = updates.as_slice() else {
        panic!("one alert expected, got {updates:?}");
    };
    assert_eq!(alert.title.as_deref(), Some("crash on start"));
    assert_eq!(
        alert.message.as_deref(),
        Some("New issue #5 in app by dana")
    );
    assert_eq!(alert.source.as_deref(), Some("github"));
    assert_eq!(
        alert.url.as_deref(),
        Some("https://github.com/me/app/issues/5")
    );
    // The same list again: nothing new.
    assert!(
        state
            .on_issues("me/app", vec![issue(5, "crash on start", false)])
            .is_empty()
    );
}

#[test]
fn parallel_jobs_are_listed_under_their_run() {
    let mut source = state(false, 0);
    let mut facts = CiFacts::default();
    apply(&mut facts, source.on_runs(REPO, fixture_runs(), NOW), NOW);
    let mut jobs = fixture_jobs();
    let mut done = jobs[0].clone();
    done.id = 2002;
    done.name = "msrv".into();
    done.status = "completed".into();
    done.conclusion = Some("failure".into());
    done.completed_at = Some("2026-10-08T18:01:10Z".into());
    done.steps[2].status = "completed".into();
    done.steps[2].conclusion = Some("failure".into());
    jobs.push(done);
    let (updates, _) = source.on_jobs(REPO, 1003, jobs, NOW + 5);
    apply(&mut facts, updates, NOW + 5);

    let group = facts.jobs()[0].1.group.clone();
    let group = group.as_deref().expect("a running job names its run");
    let list = facts.run_jobs(group).expect("the run's jobs");
    let names: Vec<_> = list.iter().map(|j| (j.name.as_str(), j.status)).collect();
    assert_eq!(
        names,
        [("test", RunStatus::Running), ("msrv", RunStatus::Failed)]
    );
    assert_eq!(list[0].step.as_deref(), Some("cargo test"));
    assert_eq!(list[1].step.as_deref(), Some("cargo test"));
    assert_eq!(
        list[1].finished.zip(list[1].started).map(|(f, s)| f - s),
        Some(60)
    );

    // The run ends: the list goes with its jobs.
    let mut runs = fixture_runs();
    runs[0].status = Some("completed".into());
    runs[0].conclusion = Some("failure".into());
    apply(&mut facts, source.on_runs(REPO, runs, NOW + 60), NOW + 60);
    assert!(facts.run_jobs(group).is_none());
}
