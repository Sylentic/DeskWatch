//! GitLab source tests: the state machine fed with fixtures written from the
//! API documentation, and the whole source against a mock GitLab.

use std::sync::Arc;

use serde_json::json;
use tokio::sync::mpsc;

use super::*;
use crate::ci::{CiFacts, RunnerStatus};
use crate::model::{Level, RowStatus, ScreenData, pick};
use crate::source::{SourceBody, SourceMsg};

/// 2026-10-08T18:02:00Z, just after the fixtures' newest pipeline started.
const NOW: u64 = 1_791_482_520;

const PROJECT: &str = "your-group/demo";
const JOB_KEY: &str = "gitlab:personal:your-group/demo:pipeline:1004";

fn fixture_mrs() -> Vec<MergeRequest> {
    serde_json::from_str(include_str!("testdata/merge_requests.json")).unwrap()
}

fn fixture_pipelines() -> Vec<Pipeline> {
    serde_json::from_str(include_str!("testdata/pipelines.json")).unwrap()
}

fn fixture_jobs() -> Vec<Job> {
    serde_json::from_str(include_str!("testdata/jobs.json")).unwrap()
}

fn state(interrupt: bool, since: u64) -> GitlabState {
    GitlabState::new(
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

#[test]
fn merge_requests_count_as_pull_requests_with_a_new_mr_flash() {
    let mut source = state(true, 0);
    let mut facts = CiFacts::default();

    // First poll: the MRs that are already open do not flash.
    apply(
        &mut facts,
        source.on_pulls(PROJECT, fixture_mrs(), None, NOW),
        NOW,
    );
    assert_eq!(facts.open_pull_count(), 2);
    assert!(facts.candidates().is_empty());
    assert_eq!(facts.pull_rows().rows[0].sub, "demo #43");

    // A new MR appears: one flash. A new draft does not flash but counts.
    let mut mrs = fixture_mrs();
    mrs.push(
        serde_json::from_value(json!({ "iid": 44, "title": "Fix login", "draft": false })).unwrap(),
    );
    mrs.push(
        serde_json::from_value(json!({ "iid": 45, "title": "Try things", "draft": true })).unwrap(),
    );
    apply(
        &mut facts,
        source.on_pulls(PROJECT, mrs, None, NOW + 60),
        NOW + 60,
    );
    assert_eq!(facts.open_pull_count(), 4);
    let Some((Level::Notice, ScreenData::Notice(notice))) = top(&facts, NOW + 60) else {
        panic!("expected a New PR notice");
    };
    assert_eq!(notice.sub, "demo #44: Fix login");
    assert_eq!(facts.candidates().len(), 1, "no flash for the draft");

    // Merged and closed MRs leave the count.
    apply(
        &mut facts,
        source.on_pulls(PROJECT, vec![], None, NOW + 120),
        NOW + 120,
    );
    assert_eq!(facts.open_pull_count(), 0);
}

#[test]
fn the_exact_total_beats_the_one_page_list() {
    let mut source = state(true, 0);
    let mut facts = CiFacts::default();
    apply(
        &mut facts,
        source.on_pulls(PROJECT, fixture_mrs(), Some(137), NOW),
        NOW,
    );
    assert_eq!(facts.open_pull_count(), 137);
    // A total below what was listed (a race) never lowers the count.
    apply(
        &mut facts,
        source.on_pulls(PROJECT, fixture_mrs(), Some(1), NOW + 60),
        NOW + 60,
    );
    assert_eq!(facts.open_pull_count(), 2);
}

#[test]
fn notify_new_prs_off() {
    let mut source = GitlabState::new(
        SourceId::new(KIND, "work"),
        Interrupt::All(true),
        BTreeMap::new(),
        false,
        0,
    );
    source.on_pulls(PROJECT, vec![], None, NOW);
    let updates = source.on_pulls(PROJECT, fixture_mrs(), None, NOW + 60);
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
        source.on_runs(PROJECT, fixture_pipelines(), NOW),
        NOW,
    );

    let page = facts.pipeline_rows();
    let rows: Vec<(&str, RowStatus)> = page
        .rows
        .iter()
        .map(|r| (r.text.as_str(), r.status))
        .collect();
    // The unnamed pipelines show the newest one (running), the older
    // cancelled one is gone. "Docs check" shows its failure, not the older green.
    assert_eq!(
        rows,
        [
            (".gitlab-ci.yml", RowStatus::Running),
            ("Docs check", RowStatus::Failed),
        ]
    );
    assert_eq!(page.rows[0].sub, "demo main");

    // Same answer again: nothing new to send.
    let again = source.on_runs(PROJECT, fixture_pipelines(), NOW + 60);
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
        source.on_runs(PROJECT, fixture_pipelines(), NOW),
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
fn fresh_failure_names_the_failing_job() {
    let mut source = state(true, 0);
    let mut facts = CiFacts::default();
    let pipelines = fixture_pipelines();
    assert_eq!(source.need_failed_step(PROJECT, &pipelines), [1003]);
    // `unit` fails the pipeline; `flaky-extra` is allowed to fail.
    let failed: Vec<Job> = serde_json::from_str(include_str!("testdata/failed_jobs.json")).unwrap();
    source.set_failed_step(PROJECT, 1003, failing_step(&failed));
    assert!(
        source.need_failed_step(PROJECT, &pipelines).is_empty(),
        "looked up once"
    );

    apply(&mut facts, source.on_runs(PROJECT, pipelines, NOW), NOW);
    let alert = facts
        .candidates()
        .into_iter()
        .find_map(|c| match c.data {
            ScreenData::Alert(a) if c.level == Level::AlertFailed => Some(a),
            _ => None,
        })
        .expect("red alert");
    assert_eq!(alert.pipeline.as_deref(), Some("Docs check"));
    assert_eq!(alert.step.as_deref(), Some("unit"));
    assert_eq!(alert.source, "gitlab");
    assert_eq!(alert.finished, Some(alert.started + 180));
}

#[test]
fn running_pipeline_shows_stage_progress_until_it_finishes() {
    let mut source = state(true, NOW);
    let mut facts = CiFacts::default();
    apply(
        &mut facts,
        source.on_runs(PROJECT, fixture_pipelines(), NOW),
        NOW,
    );
    assert_eq!(source.watched_pipelines(), [(PROJECT.to_string(), 1004)]);

    let (updates, done) = source.on_jobs(PROJECT, 1004, fixture_jobs(), NOW + 5);
    assert!(!done);
    apply(&mut facts, updates, NOW + 5);
    let Some((Level::Job, ScreenData::Job(job))) = top(&facts, NOW + 5) else {
        panic!("expected a job screen");
    };
    assert_eq!(job.source, "gitlab");
    assert_eq!(job.pipeline, ".gitlab-ci.yml");
    assert_eq!(job.git_ref, "main");
    assert_eq!(job.commit, "a1b2c3d");
    // Stages build, test, deploy; build is done, `integration` runs in test.
    assert_eq!(job.step.as_deref(), Some("test: integration"));
    assert_eq!((job.step_no, job.step_count), (Some(2), Some(3)));
    assert!((job.progress.unwrap() - 1.0 / 3.0).abs() < 1e-6);
    assert_eq!(
        job.started,
        unix_seconds(Some("2026-10-08T18:00:32.000Z")).unwrap()
    );

    // Every job ends: the pipeline result is worth fetching now.
    let mut jobs = fixture_jobs();
    for job in &mut jobs {
        job.status = "success".into();
    }
    let (updates, done) = source.on_jobs(PROJECT, 1004, jobs, NOW + 60);
    assert!(done);
    apply(&mut facts, updates, NOW + 60);

    // The next pipelines poll sees it succeeded: green flash, card gone.
    let mut pipelines = fixture_pipelines();
    pipelines[0].status = "success".into();
    pipelines[0].updated_at = Some("2026-10-08T18:03:00.000Z".into());
    apply(
        &mut facts,
        source.on_runs(PROJECT, pipelines, NOW + 61),
        NOW + 61,
    );
    assert_eq!(top(&facts, NOW + 61).unwrap().0, Level::AlertSuccess);
    assert!(!facts.has_job(JOB_KEY));
    assert!(source.watched_pipelines().is_empty());

    // A late jobs answer for the finished pipeline changes nothing.
    let (updates, _) = source.on_jobs(PROJECT, 1004, fixture_jobs(), NOW + 62);
    assert!(updates.is_empty());
}

#[test]
fn pipeline_finishing_between_polls_clears_its_card() {
    let mut source = state(true, NOW);
    let mut facts = CiFacts::default();
    apply(
        &mut facts,
        source.on_runs(PROJECT, fixture_pipelines(), NOW),
        NOW,
    );
    let (updates, _) = source.on_jobs(PROJECT, 1004, fixture_jobs(), NOW + 5);
    apply(&mut facts, updates, NOW + 5);
    assert!(facts.has_job(JOB_KEY));

    // The pipeline failed before the next jobs poll.
    let mut pipelines = fixture_pipelines();
    pipelines[0].status = "failed".into();
    pipelines[0].updated_at = Some("2026-10-08T18:03:00.000Z".into());
    apply(
        &mut facts,
        source.on_runs(PROJECT, pipelines, NOW + 60),
        NOW + 60,
    );
    assert!(!facts.has_job(JOB_KEY));
    assert_eq!(top(&facts, NOW + 60).unwrap().0, Level::AlertFailed);
}

#[test]
fn quiet_projects_are_job_polled_but_stay_off_the_esp_screen() {
    let mut source = state(false, 0);
    let mut facts = CiFacts::default();
    apply(
        &mut facts,
        source.on_runs(PROJECT, fixture_pipelines(), NOW),
        NOW,
    );
    assert_eq!(source.watched_pipelines(), [(PROJECT.to_string(), 1004)]);

    let (updates, _) = source.on_jobs(PROJECT, 1004, fixture_jobs(), NOW + 5);
    apply(&mut facts, updates, NOW + 5);
    assert!(facts.has_job(JOB_KEY));
    assert_eq!(facts.jobs().len(), 1, "the kiosk list sees it");
    assert!(facts.candidates().is_empty(), "the banner does not");
}

#[test]
fn stage_progress_between_stages_and_without_jobs() {
    // No job running yet (a runner is being found): the first open stage.
    let mut jobs = fixture_jobs();
    for job in &mut jobs {
        if job.name == "integration" {
            job.status = "pending".into();
        }
    }
    let progress = stage_progress(&jobs);
    assert_eq!(progress.step.as_deref(), Some("test"));
    assert_eq!(progress.step_no, Some(2));

    // Two jobs of the stage run: the stage name alone.
    for job in &mut jobs {
        if job.stage == "test" {
            job.status = "running".into();
        }
    }
    assert_eq!(stage_progress(&jobs).step.as_deref(), Some("test"));

    assert_eq!(stage_progress(&[]), StageProgress::default());
}

#[test]
fn deploy_pipelines_are_deploys() {
    let mut source = state(true, NOW);
    let mut pipelines = fixture_pipelines();
    pipelines[0].name = Some("Deploy to prod".into());
    source.on_runs(PROJECT, pipelines, NOW);
    let (updates, _) = source.on_jobs(PROJECT, 1004, fixture_jobs(), NOW + 5);
    let kind = updates.iter().find_map(|u| match u {
        Update::JobRunning { job, .. } => Some(job.data.kind),
        _ => None,
    });
    assert_eq!(kind, Some(crate::model::JobKind::Deploy));
}

#[test]
fn health_worst_project_wins() {
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
        ("created", RunStatus::Queued),
        ("waiting_for_resource", RunStatus::Queued),
        ("preparing", RunStatus::Queued),
        ("pending", RunStatus::Queued),
        ("scheduled", RunStatus::Queued),
        ("running", RunStatus::Running),
        ("success", RunStatus::Success),
        ("success-with-warnings", RunStatus::Success),
        ("failed", RunStatus::Failed),
        ("manual", RunStatus::Waiting),
        ("canceled", RunStatus::Neutral),
        ("canceling", RunStatus::Neutral),
        ("skipped", RunStatus::Neutral),
        ("something-new", RunStatus::Neutral),
    ];
    for (status, expected) in cases {
        assert_eq!(pipeline_status(status), expected, "{status}");
    }
}

#[test]
fn refs_read_like_branches() {
    assert_eq!(display_ref("main"), "main");
    assert_eq!(display_ref("refs/heads/feature/x"), "feature/x");
    assert_eq!(display_ref("refs/tags/v1.2"), "v1.2");
    assert_eq!(display_ref("refs/merge-requests/42/head"), "!42");
    assert_eq!(display_ref(""), "");
}

#[test]
fn backoff() {
    let poll = Duration::from_secs(60);
    assert_eq!(next_delay(poll, 0), poll);
    assert_eq!(next_delay(poll, 2), Duration::from_secs(120));
    assert_eq!(next_delay(poll, 9), Duration::from_secs(300));
}

#[test]
fn kiosk_links_come_from_gitlab_and_bad_ones_are_dropped() {
    let mut source = state(true, 0);
    let mrs: Vec<MergeRequest> = serde_json::from_value(json!([
        { "iid": 5, "title": "ok", "web_url": "https://gitlab.example.com/your-group/demo/-/merge_requests/5" },
        { "iid": 6, "title": "odd", "web_url": "javascript:alert(1)" },
        { "iid": 7, "title": "none" }
    ]))
    .unwrap();
    let updates = source.on_pulls(PROJECT, mrs, None, NOW);
    let Some(Update::Pulls { repo, .. }) = updates.last() else {
        panic!("expected the MR list");
    };
    let urls: Vec<_> = repo.pulls.iter().map(|p| p.url.as_deref()).collect();
    assert_eq!(
        urls,
        [
            Some("https://gitlab.example.com/your-group/demo/-/merge_requests/5"),
            None,
            None
        ]
    );

    let updates = source.on_runs(PROJECT, fixture_pipelines(), NOW);
    let run_url = updates.iter().find_map(|u| match u {
        Update::Run { run, .. } => run.url.clone(),
        _ => None,
    });
    assert_eq!(
        run_url.as_deref(),
        Some("https://gitlab.example.com/your-group/demo/-/pipelines/1004")
    );
    // The card of a running pipeline links to the pipeline.
    let (updates, _) = source.on_jobs(PROJECT, 1004, fixture_jobs(), NOW + 5);
    let job_url = updates.iter().find_map(|u| match u {
        Update::JobRunning { job, .. } => job.url.clone(),
        _ => None,
    });
    assert_eq!(job_url, run_url);
}

#[test]
fn runner_pages_follow_the_site_address() {
    let links = |site: &str| {
        let source = state(true, 0).with_site(site);
        let scope = |text: &str| source.runners_url(&RunnerScope::parse(text).unwrap());
        (
            scope("project:me/demo"),
            scope("group:team/sub"),
            scope("instance"),
        )
    };
    let (project, group, instance) = links("https://gitlab.com/");
    assert_eq!(
        project.as_deref(),
        Some("https://gitlab.com/me/demo/-/settings/ci_cd")
    );
    assert_eq!(
        group.as_deref(),
        Some("https://gitlab.com/groups/team/sub/-/runners")
    );
    assert_eq!(
        instance.as_deref(),
        Some("https://gitlab.com/admin/runners")
    );
    // Self-managed under a sub path, given as the API address.
    assert_eq!(
        links("https://host.example/gitlab/api/v4").0.as_deref(),
        Some("https://host.example/gitlab/me/demo/-/settings/ci_cd")
    );
}

fn source_for(config: &str) -> Gitlab {
    let config: GitlabConfig = toml::from_str(config).unwrap();
    Gitlab {
        id: SourceId::new(KIND, &config.name),
        api: HttpApi::new(&config.base_url, crate::source::Secret::new("t")).unwrap(),
        config,
    }
}

#[tokio::test]
async fn source_polls_a_mock_gitlab() {
    let seen = Arc::new(api::tests::Seen::default());
    let base_url = api::tests::mock_gitlab(seen.clone()).await;
    let source = source_for(&format!(
        "name = \"work\"\nbase_url = \"{base_url}\"\ntoken_file = \"unused\"\n\
         projects = [\"{PROJECT}\"]\njob_poll_s = 1\nalias = {{ \"{PROJECT}\" = \"Demo\" }}"
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
        assert_eq!(msg.source.to_string(), "gitlab:work");
        match msg.body {
            SourceBody::Health(h) => health = Some(h),
            SourceBody::Forget => panic!("a source never forgets itself"),
            SourceBody::Reason(_) => {}
            SourceBody::Facts(updates) => {
                for update in updates {
                    match update {
                        Update::Pulls { repo, .. } => {
                            // 137 from the X-Total header, not the 2 listed.
                            assert_eq!((repo.project.as_str(), repo.total), ("Demo", 137));
                            pulls = true;
                        }
                        Update::Run { .. } => runs = true,
                        Update::JobRunning { job: j, .. } => job = Some(j),
                        _ => {}
                    }
                }
            }
            SourceBody::Stats(_) => panic!("GitLab sends no host stats"),
        }
    }
    assert_eq!(health, Some(Health::Ok));
    assert_eq!(job.unwrap().data.step.as_deref(), Some("test: integration"));

    // The failed pipeline's failed jobs were fetched once for its failing job.
    let requests = seen.requests.lock().unwrap();
    assert!(requests.iter().any(|(what, _)| what == "failed jobs 1003"));
}

#[tokio::test]
async fn source_polls_runners_of_a_project_and_a_group() {
    let seen = Arc::new(api::tests::Seen::default());
    let base_url = api::tests::mock_gitlab(seen).await;
    let source = source_for(&format!(
        "name = \"work\"\nbase_url = \"{base_url}\"\ntoken_file = \"unused\"\n\
         projects = [\"{PROJECT}\"]\nrunners = [\"project:{PROJECT}\", \"group:your-group\"]\n\
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
            "gitlab:work:runners:group:your-group".to_string(),
            format!("gitlab:work:runners:project:{PROJECT}"),
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
            ("build-box-2", RunnerStatus::Busy),
            ("gitlab-runner", RunnerStatus::Offline),
            ("retired-box", RunnerStatus::Offline),
        ]
    );
    assert_eq!(sets[&keys[0]][0].source, "gitlab");
}

/// A token without the runner right gets 403 on the runner list only. That
/// must not make the source look broken: the PR and pipeline widgets stay fresh.
#[tokio::test]
async fn refused_runner_list_does_not_hurt_source_health() {
    use axum::Router;
    use axum::http::StatusCode;
    use axum::routing::get;

    let project = "/api/v4/projects/{id}";
    let app = Router::new()
        .route(&format!("{project}/merge_requests"), get(|| async { "[]" }))
        .route(&format!("{project}/pipelines"), get(|| async { "[]" }))
        .route(
            &format!("{project}/runners"),
            get(|| async { (StatusCode::FORBIDDEN, "{\"message\":\"403 Forbidden\"}") }),
        );
    let base_url = api::tests::serve(app).await;
    let source = source_for(&format!(
        "name = \"home\"\nbase_url = \"{base_url}\"\ntoken_file = \"unused\"\n\
         projects = [\"{PROJECT}\"]\nrunners = [\"project:{PROJECT}\"]\nrunner_poll_s = 1"
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
            SourceBody::Stats(_) => panic!("GitLab sends no host stats"),
            SourceBody::Forget => panic!("a source never forgets itself"),
            SourceBody::Reason(_) => {}
        }
    }
    assert_eq!(healths, [Health::Ok]);
}

#[test]
fn new_issues_raise_alerts_after_the_first_poll() {
    let issue = |iid: u64, title: &str| -> Issue {
        serde_json::from_value(json!({
            "iid": iid,
            "title": title,
            "web_url": format!("https://gitlab.example/group/app/-/issues/{iid}"),
            "author": { "username": "dana" },
        }))
        .unwrap()
    };
    let mut state = state(true, 0).with_issue_alerts(true);
    assert!(
        state
            .on_issues("group/app", vec![issue(3, "old")])
            .is_empty()
    );
    let updates = state.on_issues(
        "group/app",
        vec![issue(5, "five"), issue(4, "four"), issue(3, "old")],
    );
    let titles: Vec<_> = updates
        .iter()
        .map(|u| match u {
            Update::Alert { alert } => alert.title.clone().unwrap(),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(titles, ["four", "five"], "oldest first, newest on top");
    let Update::Alert { alert } = &updates[1] else {
        unreachable!()
    };
    assert_eq!(
        alert.message.as_deref(),
        Some("New issue #5 in app by dana")
    );
    assert_eq!(alert.source.as_deref(), Some("gitlab"));
    assert_eq!(
        alert.url.as_deref(),
        Some("https://gitlab.example/group/app/-/issues/5")
    );
}

#[test]
fn every_job_of_the_pipeline_is_listed_with_its_stage() {
    let mut source = state(false, 0);
    let mut facts = CiFacts::default();
    apply(
        &mut facts,
        source.on_runs(PROJECT, fixture_pipelines(), NOW),
        NOW,
    );
    let mut jobs = fixture_jobs();
    // A failure that is allowed does not count as a failure.
    jobs[2].status = "failed".into();
    jobs[2].allow_failure = true;
    let (updates, _) = source.on_jobs(PROJECT, 1004, jobs, NOW + 5);
    apply(&mut facts, updates, NOW + 5);
    let list = facts.run_jobs(JOB_KEY).expect("the pipeline's jobs");
    let rows: Vec<_> = list
        .iter()
        .map(|j| (j.name.as_str(), j.step.as_deref(), j.status))
        .collect();
    assert_eq!(
        rows,
        [
            ("lint", Some("build"), RunStatus::Success),
            ("compile", Some("build"), RunStatus::Success),
            ("unit", Some("test"), RunStatus::Neutral),
            ("integration", Some("test"), RunStatus::Running),
            ("deploy-prod", Some("deploy"), RunStatus::Queued),
        ]
    );
    assert_eq!(facts.jobs()[0].1.group.as_deref(), Some(JOB_KEY));
}
