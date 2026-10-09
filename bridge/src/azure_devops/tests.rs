//! Azure DevOps source tests: the state machine fed with the sanitised
//! fixtures, and the whole source against a mock server.

use std::sync::Arc;

use tokio::sync::mpsc;

use super::payload::{List, Timeline};
use super::*;
use crate::ci::CiFacts;
use crate::model::{Level, RowStatus, ScreenData, pick};
use crate::source::{SourceBody, SourceMsg};

/// 2026-10-08T18:02:00Z, two minutes after the running deploy started.
const NOW: u64 = 1_791_482_520;
const PROJECT: &str = "project-a";

fn fixture_builds() -> Vec<Build> {
    let list: List<Build> = serde_json::from_str(include_str!("testdata/builds.json")).unwrap();
    list.value
}

fn fixture_pulls() -> Vec<PullRequest> {
    let list: List<PullRequest> =
        serde_json::from_str(include_str!("testdata/pulls.json")).unwrap();
    list.value
}

fn records(json: &str) -> Vec<Record> {
    serde_json::from_str::<Timeline>(json).unwrap().records
}

fn config(extra: &str) -> AzureDevopsConfig {
    toml::from_str(&format!(
        "name = \"work\"\norganization = \"your-org\"\nprojects = [\"{PROJECT}\"]\n\
         token_file = \"unused\"\n{extra}"
    ))
    .unwrap()
}

fn state(extra: &str, since: u64) -> AzdoState {
    AzdoState::new(&SourceId::new(KIND, "work"), &config(extra), since)
}

fn apply(facts: &mut CiFacts, updates: Vec<Update>, now: u64) {
    for update in updates {
        facts.apply(update, now);
    }
}

/// Status of a pipeline's row on the pipelines page.
fn row_status(facts: &CiFacts, pipeline: &str) -> RowStatus {
    let page = facts.pipelines_page();
    page.rows
        .iter()
        .find(|r| r.text == pipeline)
        .unwrap_or_else(|| panic!("no row for {pipeline}"))
        .status
}

fn top(facts: &CiFacts, now: u64) -> Option<(Level, ScreenData)> {
    let candidates = facts.candidates();
    pick(&candidates, now).map(|c| (c.level, c.data.clone()))
}

#[test]
fn builds_fill_pipelines_page_with_latest_per_pipeline() {
    let mut source = state("", 0);
    let mut facts = CiFacts::default();
    apply(
        &mut facts,
        source.on_builds(PROJECT, fixture_builds(), NOW),
        NOW,
    );

    let page = facts.pipelines_page();
    let rows: Vec<(&str, RowStatus)> = page
        .rows
        .iter()
        .map(|r| (r.text.as_str(), r.status))
        .collect();
    // deploy-infra shows its running build, not the older green one.
    assert_eq!(
        rows,
        [
            ("deploy-infra", RowStatus::Running),
            ("build-app", RowStatus::Failed),
            ("nightly-checks", RowStatus::Neutral),
            ("docs", RowStatus::Neutral),
        ]
    );
    assert_eq!(page.rows[0].sub, "project-a main");
    assert_eq!(page.rows[1].sub, "project-a feature/login");

    // Same answer again: nothing new to send.
    let again = source.on_builds(PROJECT, fixture_builds(), NOW + 60);
    assert!(
        again.iter().all(|u| !matches!(u, Update::Run { .. })),
        "{again:?}"
    );
}

#[test]
fn status_mapping() {
    let build = |status: &str, result: Option<&str>| Build {
        status: Some(status.into()),
        result: result.map(Into::into),
        ..Build::default()
    };
    let cases = [
        ("notStarted", None, RunStatus::Queued),
        ("postponed", None, RunStatus::Queued),
        ("none", None, RunStatus::Queued),
        ("inProgress", None, RunStatus::Running),
        ("cancelling", None, RunStatus::Running),
        ("completed", Some("succeeded"), RunStatus::Success),
        ("completed", Some("failed"), RunStatus::Failed),
        ("completed", Some("partiallySucceeded"), RunStatus::Failed),
        ("completed", Some("canceled"), RunStatus::Neutral),
        ("completed", None, RunStatus::Neutral),
    ];
    for (status, result, expected) in cases {
        assert_eq!(
            build_status(&build(status, result)),
            expected,
            "{status} {result:?}"
        );
    }
    assert_eq!(build_status(&Build::default()), RunStatus::Queued);
}

#[test]
fn branch_names() {
    assert_eq!(branch_name(Some("refs/heads/main")), "main");
    assert_eq!(
        branch_name(Some("refs/heads/feature/login")),
        "feature/login"
    );
    assert_eq!(branch_name(Some("refs/tags/v1.2")), "v1.2");
    assert_eq!(branch_name(Some("refs/pull/12/merge")), "PR 12");
    assert_eq!(branch_name(None), "");
}

#[test]
fn builds_finished_before_startup_stay_off_screen() {
    // The deploy build failed before the bridge started: it counts in the
    // badge but does not take over the screen.
    let mut source = state("interrupt = true", NOW);
    let mut facts = CiFacts::default();
    apply(
        &mut facts,
        source.on_builds(PROJECT, fixture_builds(), NOW),
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
fn work_pipelines_do_not_interrupt_by_default() {
    let mut source = state("", 0);
    let mut facts = CiFacts::default();
    apply(
        &mut facts,
        source.on_builds(PROJECT, fixture_builds(), NOW),
        NOW,
    );
    assert_eq!(facts.failed_count(), 1, "still counted in the badge");
    assert!(
        facts.candidates().is_empty(),
        "but nothing takes the screen"
    );
    assert_eq!(
        source.watched_builds(),
        [(PROJECT.to_string(), 3004)],
        "the running build is still polled, for the kiosk list"
    );
    assert_eq!(source.live_builds(PROJECT), [3004]);
}

#[test]
fn failed_build_names_the_failing_task() {
    let mut source = state("interrupt = [\"project-a\"]", 0);
    let mut facts = CiFacts::default();
    let builds = fixture_builds();
    assert_eq!(source.need_failed_step(PROJECT, &builds), [3003]);
    let failing = timeline::analyse(&records(include_str!("testdata/timeline_failed.json")));
    source.set_failed_step(PROJECT, 3003, failing.failing_step);
    assert!(
        source.need_failed_step(PROJECT, &builds).is_empty(),
        "looked up once"
    );

    apply(&mut facts, source.on_builds(PROJECT, builds, NOW), NOW);
    let alert = facts
        .candidates()
        .into_iter()
        .find_map(|c| match c.data {
            ScreenData::Alert(a) if c.level == Level::AlertFailed => Some(a),
            _ => None,
        })
        .expect("red alert");
    assert_eq!(alert.pipeline.as_deref(), Some("build-app"));
    assert_eq!(alert.step.as_deref(), Some("Run unit tests"));
    assert_eq!(alert.source, "azure_devops");
    assert_eq!(alert.finished.unwrap() - alert.started, 270);
}

#[test]
fn partly_succeeded_build_is_a_failure() {
    let mut source = state("interrupt = true", 0);
    let mut facts = CiFacts::default();
    let mut builds = fixture_builds();
    builds[2].result = Some("partiallySucceeded".into());
    apply(&mut facts, source.on_builds(PROJECT, builds, NOW), NOW);
    assert_eq!(top(&facts, NOW).unwrap().0, Level::AlertFailed);
}

#[test]
fn running_deploy_shows_stage_task_and_progress() {
    let mut source = state("interrupt = true", NOW);
    let mut facts = CiFacts::default();
    apply(
        &mut facts,
        source.on_builds(PROJECT, fixture_builds(), NOW),
        NOW,
    );
    assert_eq!(source.watched_builds(), [(PROJECT.to_string(), 3004)]);

    let (updates, done) = source.on_timeline(
        PROJECT,
        3004,
        &records(include_str!("testdata/timeline_running.json")),
        NOW + 5,
    );
    assert!(!done);
    apply(&mut facts, updates, NOW + 5);
    let Some((Level::Job, ScreenData::Job(job))) = top(&facts, NOW + 5) else {
        panic!("expected a job screen");
    };
    assert_eq!(job.source, "azure_devops");
    assert_eq!(job.pipeline, "deploy-infra");
    assert_eq!(job.kind, JobKind::Deploy);
    assert_eq!(job.git_ref, "main");
    assert_eq!(job.commit, "abcdef1");
    assert_eq!(job.step.as_deref(), Some("apply: terraform apply"));
    assert_eq!((job.step_no, job.step_count), (Some(4), Some(5)));
    assert!((job.progress.unwrap() - 0.8).abs() < 1e-6);
    assert_eq!(job.started, 1_791_482_400);

    // The next poll lists the build as succeeded: green flash, job gone.
    let mut builds = fixture_builds();
    builds[1].status = Some("completed".into());
    builds[1].result = Some("succeeded".into());
    builds[1].finish_time = Some("2026-10-08T18:03:00Z".into());
    apply(
        &mut facts,
        source.on_builds(PROJECT, builds, NOW + 60),
        NOW + 60,
    );
    assert_eq!(top(&facts, NOW + 60).unwrap().0, Level::AlertSuccess);
    assert!(source.watched_builds().is_empty());
    assert!(!facts.has_job("azure_devops:work:project-a:job:3004"));

    // A late timeline answer for the finished build changes nothing.
    let (updates, _) = source.on_timeline(
        PROJECT,
        3004,
        &records(include_str!("testdata/timeline_running.json")),
        NOW + 62,
    );
    assert!(updates.is_empty());
}

#[test]
fn deploy_waiting_for_approval_is_a_notice_and_a_review_row() {
    let mut source = state("interrupt = true", NOW);
    let mut facts = CiFacts::default();
    apply(
        &mut facts,
        source.on_builds(PROJECT, fixture_builds(), NOW),
        NOW,
    );
    let (updates, _) = source.on_timeline(
        PROJECT,
        3004,
        &records(include_str!("testdata/timeline_running.json")),
        NOW + 5,
    );
    apply(&mut facts, updates, NOW + 5);
    assert_eq!(top(&facts, NOW + 5).unwrap().0, Level::Job);

    // The apply stage now waits for an approval: one notice, the job screen
    // is withdrawn and the pipeline shows as `review`.
    let approval = records(include_str!("testdata/timeline_approval.json"));
    let (updates, _) = source.on_timeline(PROJECT, 3004, &approval, NOW + 10);
    apply(&mut facts, updates, NOW + 10);
    let Some((Level::Notice, ScreenData::Notice(notice))) = top(&facts, NOW + 10) else {
        panic!("expected an approval notice");
    };
    assert_eq!(notice.text, "Approval needed");
    assert_eq!(notice.sub, "project-a deploy-infra");
    assert_eq!(facts.candidates().len(), 1, "no job screen while waiting");
    assert_eq!(row_status(&facts, "deploy-infra"), RowStatus::Review);

    // Polling again while it still waits does not flash again.
    facts.dismiss_notices();
    let (updates, _) = source.on_timeline(PROJECT, 3004, &approval, NOW + 15);
    apply(&mut facts, updates, NOW + 15);
    assert!(facts.candidates().is_empty());
    // Nor does the regular builds poll undo the review status.
    apply(
        &mut facts,
        source.on_builds(PROJECT, fixture_builds(), NOW + 60),
        NOW + 60,
    );
    assert_eq!(row_status(&facts, "deploy-infra"), RowStatus::Review);

    // Approved: running again, job screen back.
    let (updates, _) = source.on_timeline(
        PROJECT,
        3004,
        &records(include_str!("testdata/timeline_running.json")),
        NOW + 70,
    );
    apply(&mut facts, updates, NOW + 70);
    assert_eq!(top(&facts, NOW + 70).unwrap().0, Level::Job);
    assert_eq!(row_status(&facts, "deploy-infra"), RowStatus::Running);
}

#[test]
fn approval_is_seen_even_when_the_pipeline_may_not_interrupt() {
    let mut source = state("", 0);
    let mut facts = CiFacts::default();
    apply(
        &mut facts,
        source.on_builds(PROJECT, fixture_builds(), NOW),
        NOW,
    );
    let approval = records(include_str!("testdata/timeline_approval.json"));
    let (updates, _) = source.on_timeline(PROJECT, 3004, &approval, NOW + 5);
    apply(&mut facts, updates, NOW + 5);
    assert_eq!(row_status(&facts, "deploy-infra"), RowStatus::Review);
    assert_eq!(top(&facts, NOW + 5).unwrap().0, Level::Notice);
}

#[test]
fn quiet_pipeline_job_reaches_the_kiosk_list_not_the_screen() {
    let mut source = state("", 0);
    let mut facts = CiFacts::default();
    apply(
        &mut facts,
        source.on_builds(PROJECT, fixture_builds(), NOW),
        NOW,
    );
    let running = records(include_str!("testdata/timeline_running.json"));
    let (updates, _) = source.on_timeline(PROJECT, 3004, &running, NOW + 5);
    apply(&mut facts, updates, NOW + 5);
    assert_eq!(facts.jobs().len(), 1, "listed on the kiosk page");
    assert!(
        facts.candidates().is_empty(),
        "but nothing takes the screen"
    );
}

#[test]
fn build_finishing_between_timeline_polls_clears_its_job() {
    let mut source = state("interrupt = true", NOW);
    let mut facts = CiFacts::default();
    apply(
        &mut facts,
        source.on_builds(PROJECT, fixture_builds(), NOW),
        NOW,
    );
    let (updates, _) = source.on_timeline(
        PROJECT,
        3004,
        &records(include_str!("testdata/timeline_running.json")),
        NOW + 5,
    );
    apply(&mut facts, updates, NOW + 5);
    assert!(facts.has_job("azure_devops:work:project-a:job:3004"));

    let mut builds = fixture_builds();
    builds[1].status = Some("completed".into());
    builds[1].result = Some("failed".into());
    builds[1].finish_time = Some("2026-10-08T18:03:00Z".into());
    apply(
        &mut facts,
        source.on_builds(PROJECT, builds, NOW + 60),
        NOW + 60,
    );
    assert!(!facts.has_job("azure_devops:work:project-a:job:3004"));
    assert_eq!(top(&facts, NOW + 60).unwrap().0, Level::AlertFailed);
}

#[test]
fn every_stage_completed_asks_for_an_early_poll() {
    let mut source = state("interrupt = true", NOW);
    source.on_builds(PROJECT, fixture_builds(), NOW);
    let (_, done) = source.on_timeline(
        PROJECT,
        3004,
        &records(include_str!("testdata/timeline_failed.json")),
        NOW + 5,
    );
    assert!(done);
}

#[test]
fn pulls_count_per_repository_and_new_pr_flash() {
    let mut source = state("", 0);
    let mut facts = CiFacts::default();

    // First poll: the PRs that are already open do not flash.
    apply(
        &mut facts,
        source.on_pulls(PROJECT, fixture_pulls(), NOW),
        NOW,
    );
    assert_eq!(facts.open_pull_count(), 3);
    assert!(facts.candidates().is_empty());
    let page = facts.pulls_page();
    assert_eq!(page.rows[0].sub, "repo-a #43", "newest first");

    // A new PR appears: one flash. A new draft does not flash but counts.
    let mut pulls = fixture_pulls();
    for (id, draft) in [(44, false), (45, true)] {
        pulls.push(
            serde_json::from_value(serde_json::json!({
                "pullRequestId": id, "title": format!("pr {id}"), "isDraft": draft,
                "repository": { "name": "repo-b" }
            }))
            .unwrap(),
        );
    }
    apply(
        &mut facts,
        source.on_pulls(PROJECT, pulls, NOW + 60),
        NOW + 60,
    );
    assert_eq!(facts.open_pull_count(), 5);
    let Some((Level::Notice, ScreenData::Notice(notice))) = top(&facts, NOW + 60) else {
        panic!("expected a New PR notice");
    };
    assert_eq!(notice.sub, "repo-b #44: pr 44");
    assert_eq!(facts.candidates().len(), 1, "no flash for the draft");

    // Merged and abandoned PRs leave the count, also when a repository
    // has none left.
    let only_a: Vec<PullRequest> = fixture_pulls()
        .into_iter()
        .filter(|p| p.repository.name == "repo-a")
        .collect();
    apply(
        &mut facts,
        source.on_pulls(PROJECT, only_a, NOW + 120),
        NOW + 120,
    );
    assert_eq!(facts.open_pull_count(), 2);
    apply(
        &mut facts,
        source.on_pulls(PROJECT, vec![], NOW + 180),
        NOW + 180,
    );
    assert_eq!(facts.open_pull_count(), 0);
}

#[test]
fn notify_new_prs_off_and_aliases() {
    let mut source = state(
        "notify_new_prs = false\nalias = { \"project-a\" = \"Infra\", \"project-a/repo-a\" = \"A\" }",
        0,
    );
    source.on_pulls(PROJECT, vec![], NOW);
    let updates = source.on_pulls(PROJECT, fixture_pulls(), NOW + 60);
    assert!(
        updates
            .iter()
            .all(|u| !matches!(u, Update::PullOpened { .. }))
    );
    let labels: Vec<&str> = updates
        .iter()
        .filter_map(|u| match u {
            Update::Pulls { repo, .. } => Some(repo.project.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(labels, ["A", "repo-b"]);

    let mut facts = CiFacts::default();
    apply(
        &mut facts,
        source.on_builds(PROJECT, fixture_builds(), NOW),
        NOW,
    );
    assert_eq!(facts.pipelines_page().rows[0].sub, "Infra main");
}

#[test]
fn deploy_words_decide_the_kind() {
    let source = state("", 0);
    assert_eq!(source.kind_of("deploy-infra"), JobKind::Deploy);
    assert_eq!(source.kind_of("Terraform-Plan"), JobKind::Deploy);
    assert_eq!(source.kind_of("build-app"), JobKind::Build);
    let custom = state("deploy_words = [\"Release\"]", 0);
    assert_eq!(custom.kind_of("weekly-release"), JobKind::Deploy);
    assert_eq!(custom.kind_of("deploy-infra"), JobKind::Build);
}

#[test]
fn health_worst_project_wins() {
    let mut source = state("", 0);
    assert_eq!(source.health(), None);
    source.set_health("a", Health::Ok);
    source.set_health("b", Health::AuthFailed);
    assert_eq!(source.health(), Some(Health::AuthFailed));
    source.set_health("b", Health::Ok);
    assert_eq!(source.health(), Some(Health::Ok));
}

#[test]
fn backoff() {
    let poll = Duration::from_secs(60);
    assert_eq!(next_delay(poll, 0), poll);
    assert_eq!(next_delay(poll, 2), Duration::from_secs(120));
    assert_eq!(next_delay(poll, 9), Duration::from_secs(300));
}

/// Run a source against a mock server, with a short job poll.
fn start(base_url: &str) -> mpsc::Receiver<SourceMsg> {
    let config = config(&format!(
        "base_url = \"{base_url}\"\ninterrupt = true\njob_poll_s = 1\n"
    ));
    let source = AzureDevops {
        id: SourceId::new(KIND, &config.name),
        api: HttpApi::new(
            &config.base_url,
            &config.organization,
            crate::source::Secret::new("test-pat"),
        )
        .unwrap(),
        config,
    };
    let (tx, rx) = mpsc::channel(64);
    crate::source::spawn(source, tx);
    rx
}

#[tokio::test]
async fn source_polls_a_mock_service() {
    let seen = Arc::new(api::tests::Seen::default());
    let mut rx = start(&api::tests::mock_azure(seen.clone()).await);

    let mut health = None;
    let (mut pulls, mut failed_step, mut job) = (0, None, None);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !(pulls == 2 && failed_step.is_some() && job.is_some() && health.is_some()) {
        let msg: SourceMsg = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .expect("source reported everything in time")
            .unwrap();
        assert_eq!(msg.source.to_string(), "azure_devops:work");
        match msg.body {
            SourceBody::Health(h) => health = Some(h),
            SourceBody::Facts(updates) => {
                for update in updates {
                    match update {
                        Update::Pulls { repo, .. } => {
                            assert!(["repo-a", "repo-b"].contains(&repo.project.as_str()));
                            pulls += 1;
                        }
                        Update::Run { run, .. } if run.status == RunStatus::Failed => {
                            failed_step = run.step;
                        }
                        Update::JobRunning { job: j, .. } => job = Some(j),
                        _ => {}
                    }
                }
            }
            SourceBody::Stats(_) => panic!("Azure DevOps sends no host stats"),
        }
    }
    assert_eq!(health, Some(Health::Ok));
    assert_eq!(failed_step.as_deref(), Some("Run unit tests"));
    assert_eq!(
        job.unwrap().data.step.as_deref(),
        Some("apply: terraform apply")
    );

    // Every request carried the token and the API version.
    let requests = seen.requests.lock().unwrap();
    assert!(requests.iter().any(|(path, ..)| path == "pulls"));
    assert!(
        requests
            .iter()
            .all(
                |(_, query, headers)| headers["authorization"] == api::tests::BASIC_AUTH
                    && query.contains("api-version=7.1")
            )
    );
}

#[tokio::test]
async fn a_refused_token_lights_the_warn_badge() {
    use axum::Router;
    use axum::http::StatusCode;
    use axum::routing::get;

    let app = Router::new().fallback(get(|| async { StatusCode::UNAUTHORIZED }));
    let mut rx = start(&api::tests::serve(app).await);
    let msg = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("health reported")
        .unwrap();
    assert!(matches!(msg.body, SourceBody::Health(Health::AuthFailed)));
}

#[tokio::test]
async fn an_unreachable_service_lights_the_warn_badge() {
    // Nothing listens on port 9 of localhost.
    let mut rx = start("http://127.0.0.1:9");
    let msg = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("health reported")
        .unwrap();
    assert!(matches!(msg.body, SourceBody::Health(Health::Unreachable)));
}
