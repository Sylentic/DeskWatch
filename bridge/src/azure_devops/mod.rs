//! Azure DevOps source: pipeline runs and open PRs from Azure DevOps
//! Services, turned into the same CI facts as Gitea and GitHub.
//!
//! The service cannot reach a home server, so this source only polls
//! outbound (integrations plan section 6):
//!
//! - every `poll_s` (60 s), per project: the latest builds, the timeline of
//!   every build in progress (to see a stage waiting for an approval), and
//!   the open PRs of all repositories. The result is also the source's
//!   health: a refused token or an unreachable service shows as the `warn`
//!   badge.
//! - every `job_poll_s` (5 s), only while a build is in progress: that
//!   build's timeline, for the step progress bar. Projects that may not
//!   interrupt are polled too; their job shows on the kiosk page only.
//!
//! - every `agent_poll_s` (60 s), only when `agent_pools` is set: the agents
//!   of those self-hosted pools, as runners for the kiosk, and how many jobs
//!   wait for an agent in each pool. This does not
//!   touch the source's health, because a token without the Agent Pools
//!   right gets a plain 403 on this list only.
//!
//! Failures back off (60 s, 2 min, 5 min).
//!
//! Terraform pipelines are YAML pipelines with stages such as `plan` and
//! `apply`, so a run shows as "apply: terraform apply". A stage waiting for an
//! approval raises one "Approval needed" notice and a `review` row on the
//! pipelines page; it does not hold the screen. Another check on the stage
//! (branch control, business hours) does the same with "Check pending", but
//! only after two timeline polls in a row saw it, because most pass at once.

pub mod api;
pub mod capture;
pub mod payload;
mod sanitise;
pub mod timeline;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::time::{Instant, MissedTickBehavior};
use tracing::{debug, info, warn};

use crate::alerts::{self, AlertMessage};
use crate::ci::{OpenPull, RepoPulls, Run, RunStatus, Runner, RunnerStatus, RunningJob, Update};
use crate::config::AzureDevopsConfig;
use crate::gitea::payload::unix_seconds;
use crate::model::{JobData, JobKind, MAX_TITLE_CHARS, NoticeData, truncate};
use crate::source::{self, FactSink, Health, Interrupt, Source, SourceId, unix_now};
use api::HttpApi;
use payload::{Agent, Build, PullRequest, Record, Thread};
use timeline::View;

/// Adapter type: `[[source.azure_devops]]` in the config and `source` on panel pages.
pub const KIND: &str = "azure_devops";

/// A check other than an approval (branch control, business hours) has to be
/// seen in progress by this many timeline polls in a row before the build
/// counts as waiting. Most of them pass within seconds, and a flash of
/// "Check pending" on every deploy would be noise.
const CHECK_POLLS: u8 = 2;

/// Most open PRs per project whose comments are checked each poll, newest
/// first, so a busy project does not cost hundreds of requests a minute.
const MAX_COMMENT_PULLS: usize = 30;

/// Longest comment text kept for the alert line.
const COMMENT_CHARS: usize = 80;

/// Poll delays after 1, 2, and 3 or more failed rounds in a row.
const BACKOFF: [Duration; 3] = [
    Duration::from_secs(60),
    Duration::from_secs(120),
    Duration::from_secs(300),
];

// ---------------------------------------------------------------------------
// The source task
// ---------------------------------------------------------------------------

/// One `[[source.azure_devops]]` instance, ready to run.
pub struct AzureDevops {
    id: SourceId,
    config: AzureDevopsConfig,
    api: HttpApi,
}

impl AzureDevops {
    /// Load the token. Fails when the credential file is missing.
    pub fn new(config: &AzureDevopsConfig) -> Result<Self> {
        let id = SourceId::new(KIND, &config.name);
        let token =
            source::load_secret(&config.token_file).with_context(|| format!("{id}: token_file"))?;
        let api = HttpApi::new(&config.base_url, &config.organization, token)
            .context("Azure DevOps API")?;
        Ok(Self {
            id,
            config: config.clone(),
            api,
        })
    }
}

impl Source for AzureDevops {
    fn id(&self) -> SourceId {
        self.id.clone()
    }

    async fn run(self, sink: FactSink) -> Result<()> {
        let AzureDevops { id, config, api } = self;
        info!(source = %id, projects = config.projects.len(), "polling Azure DevOps");
        let poll = Duration::from_secs(config.poll_s);
        let mut state = AzdoState::new(&id, &config, unix_now());
        if state.alert_pr_comments {
            match api.me().await {
                Ok(me) => state.set_me(me),
                Err(err) => {
                    warn!(source = %id, "cannot tell whose token this is, own PR comments will alert too: {err:#}")
                }
            }
        }

        let mut next_poll = Instant::now();
        let mut failures = 0;
        let mut job_ticker = tokio::time::interval(Duration::from_secs(config.job_poll_s));
        job_ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut agent_ticker = tokio::time::interval(Duration::from_secs(config.agent_poll_s));
        agent_ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut reported = None;
        loop {
            let updates = tokio::select! {
                // Skipped altogether when no agent pools are configured.
                _ = agent_ticker.tick(), if !config.agent_pools.is_empty() => {
                    poll_agent_pools(&api, &id, &config.agent_pools).await
                }
                _ = tokio::time::sleep_until(next_poll) => {
                    let mut updates = Vec::new();
                    for project in &config.projects {
                        updates.extend(poll_project(&api, &mut state, project, config.pull_requests).await);
                    }
                    failures = match state.health() {
                        Some(Health::Ok) | None => 0,
                        _ => failures + 1,
                    };
                    next_poll = Instant::now() + next_delay(poll, failures);
                    updates
                }
                _ = job_ticker.tick() => {
                    let (updates, builds_done) = poll_timelines(&api, &mut state).await;
                    // Every stage of a build finished: fetch its result now
                    // instead of waiting for the next regular poll.
                    if builds_done {
                        next_poll = next_poll.min(Instant::now());
                    }
                    updates
                }
            };
            if !sink.facts(updates).await {
                break; // main loop has stopped
            }
            if let Some(health) = state.health()
                && reported != Some(health)
            {
                reported = Some(health);
                sink.health(health).await;
            }
        }
        Ok(())
    }
}

/// Poll the agents of the configured self-hosted pools. A failure keeps the
/// last list (the kiosk greys it out when it gets old); a 403 usually means
/// the token lacks Agent Pools (Read), see docs/runners.md.
async fn poll_agent_pools(api: &HttpApi, id: &SourceId, names: &[String]) -> Vec<Update> {
    let pools = match api.pools().await {
        Ok(pools) => pools,
        Err(err) => {
            warn!("cannot poll agent pools: {err:#}");
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for name in names {
        // Microsoft-hosted pools cannot be down, so they never match.
        let Some(pool) = pools.iter().find(|p| !p.is_hosted && p.name == *name) else {
            warn!(pool = %name, "no self-hosted agent pool with this name");
            continue;
        };
        match api.agents(pool.id).await {
            Ok(agents) => {
                debug!(pool = %name, count = agents.len(), "polled agents");
                let url = api.pool_url(pool.id);
                out.push(Update::Runners {
                    key: format!("{id}:runners:pool:{name}"),
                    runners: agents
                        .into_iter()
                        .map(|a| agent_fact(a, name, url.clone()))
                        .collect(),
                });
            }
            Err(err) => warn!(pool = %name, "cannot poll agents: {err:#}"),
        }
        match api.job_requests(pool.id).await {
            Ok(requests) => {
                let queued = requests.iter().filter(|r| r.is_queued()).count() as u32;
                debug!(pool = %name, queued, "polled job requests");
                out.push(Update::QueuedJobs {
                    key: format!("{id}:queue:pool:{name}"),
                    source: KIND.into(),
                    pool: truncate(name, MAX_TITLE_CHARS),
                    queued,
                });
            }
            Err(err) => warn!(pool = %name, "cannot poll job requests: {err:#}"),
        }
    }
    out
}

/// The shared runner fact for one agent. A status nobody knows counts as
/// offline: showing an agent as idle when we cannot tell would hide an outage.
/// Agents have no labels; the pool name stands in, so the widget shows where
/// an agent lives.
fn agent_fact(agent: Agent, pool: &str, url: Option<String>) -> Runner {
    let status = match agent.status.as_str() {
        "online" if agent.assigned_request.is_some() => RunnerStatus::Busy,
        "online" => RunnerStatus::Idle,
        _ => RunnerStatus::Offline,
    };
    Runner {
        source: KIND.into(),
        name: truncate(&agent.name, MAX_TITLE_CHARS),
        status,
        disabled: agent.enabled == Some(false),
        labels: vec![truncate(pool, MAX_TITLE_CHARS)],
        url,
    }
}

/// How long to wait before the next regular poll.
fn next_delay(poll: Duration, failures: usize) -> Duration {
    match failures {
        0 => poll,
        n => poll.max(BACKOFF[(n - 1).min(BACKOFF.len() - 1)]),
    }
}

/// Poll the builds and open PRs of one project.
async fn poll_project(
    api: &HttpApi,
    state: &mut AzdoState,
    project: &str,
    pull_requests: bool,
) -> Vec<Update> {
    let mut out = Vec::new();
    let builds = match api.builds(project).await {
        Ok(builds) => builds,
        Err(err) => {
            warn!(%project, "cannot poll Azure DevOps builds: {err:#}");
            state.set_health(project, Health::from_error(&err));
            return out;
        }
    };
    debug!(%project, builds = builds.len(), "polled builds");

    // Name the failing step in the red alert: one timeline request per
    // failed build, only the first time it is seen.
    for build_id in state.need_failed_step(project, &builds) {
        let step = match api.timeline(project, build_id).await {
            Ok(records) => timeline::analyse(&records).failing_step,
            Err(err) => {
                warn!(%project, build_id, "cannot fetch timeline of failed build: {err:#}");
                None
            }
        };
        state.set_failed_step(project, build_id, step);
    }
    out.extend(state.on_builds(project, builds, unix_now()));

    // Builds in progress: is a stage waiting for an approval?
    for build_id in state.live_builds(project) {
        match api.timeline(project, build_id).await {
            Ok(records) => out.extend(state.on_timeline(project, build_id, &records, unix_now()).0),
            // Keep what is known; the next round tries again.
            Err(err) => warn!(%project, build_id, "cannot poll timeline: {err:#}"),
        }
    }

    if pull_requests {
        match api.pulls(project).await {
            Ok(pulls) => {
                debug!(%project, open = pulls.len(), "polled open PRs");
                let watch: Vec<(String, u64, String)> = pulls
                    .iter()
                    .take(MAX_COMMENT_PULLS)
                    .map(|p| {
                        (
                            p.repository.name.clone(),
                            p.pull_request_id,
                            p.title.clone(),
                        )
                    })
                    .collect();
                out.extend(state.on_pulls(project, pulls, unix_now()));
                if state.alert_pr_comments {
                    state.keep_comments_of(project, &watch);
                    for (repo, pull_id, title) in &watch {
                        match api.threads(project, repo, *pull_id).await {
                            Ok(threads) => out
                                .extend(state.on_threads(project, repo, *pull_id, title, &threads)),
                            // Alerts only: a failure here never marks the source broken.
                            Err(err) => {
                                debug!(%project, %repo, pull_id, "cannot poll PR threads: {err:#}")
                            }
                        }
                    }
                }
            }
            Err(err) => {
                warn!(%project, "cannot poll Azure DevOps PRs: {err:#}");
                state.set_health(project, Health::from_error(&err));
                return out;
            }
        }
    }
    state.set_health(project, Health::Ok);
    out
}

/// Poll the timeline of every build that may take over the screen. Returns
/// the updates and whether some build has all its stages finished.
async fn poll_timelines(api: &HttpApi, state: &mut AzdoState) -> (Vec<Update>, bool) {
    let mut out = Vec::new();
    let mut builds_done = false;
    for (project, build_id) in state.watched_builds() {
        match api.timeline(&project, build_id).await {
            Ok(records) => {
                let (updates, done) = state.on_timeline(&project, build_id, &records, unix_now());
                out.extend(updates);
                builds_done |= done;
            }
            // Keep the last known progress; the next tick tries again.
            Err(err) => warn!(%project, build_id, "cannot poll timeline: {err:#}"),
        }
    }
    (out, builds_done)
}

// ---------------------------------------------------------------------------
// Turning API answers into facts
// ---------------------------------------------------------------------------

/// A build in progress, remembered between polls.
#[derive(Debug)]
struct LiveBuild {
    /// Key of the pipeline's `Run` fact, for fixing its status.
    run_key: String,
    pipeline: String,
    git_ref: String,
    commit: String,
    started: u64,
    kind: JobKind,
    /// May this build take over the screen?
    interrupt: bool,
    /// A stage waits for an approval, or for another check that has held for
    /// `CHECK_POLLS` timeline polls in a row.
    waiting: bool,
    /// Timeline polls in a row that saw a non-approval check in progress.
    check_polls: u8,
    /// A job screen for this build was sent and not yet withdrawn.
    job_shown: bool,
    /// Web page of the build.
    url: Option<String>,
}

/// State the Azure DevOps source keeps between polls.
#[derive(Debug)]
pub struct AzdoState {
    id: SourceId,
    interrupt: Interrupt,
    alias: BTreeMap<String, String>,
    notify_new_prs: bool,
    /// Raise an alert for each new PR comment.
    alert_pr_comments: bool,
    /// Identity id of the token's user; their comments never alert.
    me: Option<String>,
    /// Comment ids (thread, comment) seen per project, repository and PR. A
    /// PR is missing until its first look, so old comments do not alert.
    comments: HashMap<(String, String, u64), HashSet<(u64, u64)>>,
    /// Lower-case words that make a pipeline a deploy.
    deploy_words: Vec<String>,
    /// Unix seconds when the source started. Builds that finished before then
    /// show on the pipelines page and in the badge, but do not take over the
    /// screen: they are old news after a bridge restart.
    since: u64,
    /// PR numbers per project and repository from the last poll. A project is
    /// missing before its first poll, so existing PRs do not all flash
    /// "New PR" at startup.
    known_pulls: HashMap<String, HashMap<String, HashSet<u64>>>,
    /// Last `Run` sent per pipeline key, with the build it describes.
    runs: HashMap<String, (Run, u64)>,
    /// Builds in progress, by project and build id.
    live: BTreeMap<(String, u64), LiveBuild>,
    /// First failing step per failed build (`None`: looked up, none found).
    failed_steps: HashMap<(String, u64), Option<String>>,
    /// Site address and organisation, for links on the kiosk.
    web: (String, String),
    /// Result of the last poll per project.
    health: BTreeMap<String, Health>,
}

impl AzdoState {
    pub fn new(id: &SourceId, config: &AzureDevopsConfig, since: u64) -> Self {
        Self {
            id: id.clone(),
            interrupt: config.interrupt.clone(),
            alias: config.alias.clone(),
            notify_new_prs: config.notify_new_prs,
            alert_pr_comments: config.pull_requests && config.alert_pr_comments,
            me: None,
            comments: HashMap::new(),
            deploy_words: config
                .deploy_words
                .iter()
                .map(|w| w.to_lowercase())
                .collect(),
            since,
            known_pulls: HashMap::new(),
            runs: HashMap::new(),
            live: BTreeMap::new(),
            failed_steps: HashMap::new(),
            web: (config.base_url.clone(), config.organization.clone()),
            health: BTreeMap::new(),
        }
    }

    pub fn set_me(&mut self, id: String) {
        self.me = Some(id).filter(|id| !id.is_empty());
    }

    /// Forget the comments of PRs in `project` that are no longer watched
    /// (closed, merged, or past `MAX_COMMENT_PULLS`).
    pub fn keep_comments_of(&mut self, project: &str, watch: &[(String, u64, String)]) {
        self.comments.retain(|(p, repo, id), _| {
            p != project || watch.iter().any(|(r, i, _)| r == repo && i == id)
        });
    }

    /// The threads of one PR from a poll: an alert for each comment by
    /// someone else that was not there at the previous look.
    pub fn on_threads(
        &mut self,
        project: &str,
        repo: &str,
        pull_id: u64,
        title: &str,
        threads: &[Thread],
    ) -> Vec<Update> {
        let key = (project.to_string(), repo.to_string(), pull_id);
        let first_look = !self.comments.contains_key(&key);
        let seen = self.comments.entry(key).or_default();
        let mut out = Vec::new();
        for thread in threads.iter().filter(|t| !t.is_deleted) {
            for comment in &thread.comments {
                if !seen.insert((thread.id, comment.id)) || first_look {
                    continue;
                }
                let by_me = self.me.as_deref() == Some(comment.author.id.as_str());
                if comment.is_deleted || comment.comment_type != "text" || by_me {
                    continue;
                }
                let text = truncate(comment.content.trim(), COMMENT_CHARS);
                out.push(Update::Alert {
                    alert: AlertMessage::activity(
                        alerts::activity_id(&format!(
                            "{}:comment:{project}/{repo}!{pull_id}:{}:{}",
                            self.id, thread.id, comment.id
                        )),
                        format!("{} on !{pull_id} {title}", comment.author.display_name),
                        text,
                        KIND,
                        pull_url(&self.web, project, repo, pull_id)
                            .map(|url| format!("{url}?discussionId={}", thread.id)),
                    ),
                });
            }
        }
        out
    }

    /// Health from the polls: the worst project wins. `None` before the first poll.
    pub fn health(&self) -> Option<Health> {
        self.health.values().max().copied()
    }

    pub fn set_health(&mut self, project: &str, health: Health) {
        self.health.insert(project.to_string(), health);
    }

    /// Builds in progress in a project, in a stable order.
    pub fn live_builds(&self, project: &str) -> Vec<u64> {
        self.live
            .keys()
            .filter(|(p, _)| p == project)
            .map(|(_, id)| *id)
            .collect()
    }

    /// Builds whose timeline to poll every few seconds: every one in progress.
    /// Quiet projects are polled too, so the kiosk's "Running now" list sees
    /// them; only `interrupt` decides whether they take over the ESP screen.
    pub fn watched_builds(&self) -> Vec<(String, u64)> {
        self.live.keys().cloned().collect()
    }

    /// Open PRs of one project from a poll, grouped by repository.
    pub fn on_pulls(&mut self, project: &str, pulls: Vec<PullRequest>, now: u64) -> Vec<Update> {
        let mut out = Vec::new();
        let mut by_repo: BTreeMap<&str, Vec<&PullRequest>> = BTreeMap::new();
        for pull in &pulls {
            by_repo
                .entry(pull.repository.name.as_str())
                .or_default()
                .push(pull);
        }
        let web = self.web.clone();
        let first_poll = !self.known_pulls.contains_key(project);
        let known = self.known_pulls.entry(project.to_string()).or_default();

        // New since the last poll: flash, unless it is the first poll or a draft.
        if !first_poll && self.notify_new_prs {
            for (repo, list) in &by_repo {
                let seen = known.get(*repo);
                for pull in list.iter().filter(|p| {
                    !p.is_draft && !seen.is_some_and(|s| s.contains(&p.pull_request_id))
                }) {
                    out.push(Update::PullOpened {
                        key: pulls_key(&self.id, project, repo),
                        project: source::label(&self.alias, &format!("{project}/{repo}")),
                        source: KIND.into(),
                        pull: open_pull(
                            pull,
                            now,
                            pull_url(&web, project, repo, pull.pull_request_id),
                        ),
                        notify: true,
                    });
                }
            }
        }

        // Repositories that had PRs and have none now still need an update.
        let gone: Vec<String> = known
            .keys()
            .filter(|repo| !by_repo.contains_key(repo.as_str()))
            .cloned()
            .collect();
        *known = by_repo
            .iter()
            .map(|(repo, list)| {
                let numbers = list.iter().map(|p| p.pull_request_id).collect();
                (repo.to_string(), numbers)
            })
            .collect();
        known.extend(gone.iter().map(|repo| (repo.clone(), HashSet::new())));

        let repos = by_repo
            .iter()
            .map(|(repo, list)| (repo.to_string(), list.clone()))
            .chain(gone.into_iter().map(|repo| (repo, Vec::new())));
        for (repo, list) in repos {
            let open: Vec<OpenPull> = list
                .iter()
                .map(|p| open_pull(p, now, pull_url(&web, project, &repo, p.pull_request_id)))
                .collect();
            out.push(Update::Pulls {
                key: pulls_key(&self.id, project, &repo),
                repo: RepoPulls {
                    project: source::label(&self.alias, &format!("{project}/{repo}")),
                    source: KIND.into(),
                    total: open.len() as u32,
                    pulls: open,
                },
            });
        }
        out
    }

    /// Failed builds, latest of their pipeline, whose failing step has not
    /// been looked up yet.
    pub fn need_failed_step(&self, project: &str, builds: &[Build]) -> Vec<u64> {
        latest_per_pipeline(builds)
            .into_iter()
            .filter(|b| build_status(b) == RunStatus::Failed)
            .filter(|b| !self.failed_steps.contains_key(&(project.to_string(), b.id)))
            .map(|b| b.id)
            .collect()
    }

    pub fn set_failed_step(&mut self, project: &str, build_id: u64, step: Option<String>) {
        self.failed_steps
            .insert((project.to_string(), build_id), step);
    }

    /// Recent builds of one project from a poll, newest first.
    pub fn on_builds(&mut self, project: &str, builds: Vec<Build>, now: u64) -> Vec<Update> {
        let mut out = Vec::new();
        let may_interrupt = self.interrupt.allows(project);

        for build in latest_per_pipeline(&builds) {
            let pipeline = build.definition.name.clone();
            let mut status = build_status(build);
            // The timeline poll found a stage waiting for an approval.
            let waiting = self
                .live
                .get(&(project.to_string(), build.id))
                .is_some_and(|l| l.waiting);
            if status == RunStatus::Running && waiting {
                status = RunStatus::Waiting;
            }
            let started = unix_seconds(build.start_time.as_deref())
                .or_else(|| unix_seconds(build.queue_time.as_deref()))
                .unwrap_or(now);
            let finished = (build.status.as_deref() == Some("completed"))
                .then(|| unix_seconds(build.finish_time.as_deref()).unwrap_or(now));
            let fresh = finished.is_none_or(|f| f >= self.since);
            let step = self
                .failed_steps
                .get(&(project.to_string(), build.id))
                .cloned()
                .flatten()
                .filter(|_| status == RunStatus::Failed);
            let fact = Run {
                source: KIND.into(),
                project: source::label(&self.alias, project),
                pipeline: pipeline.clone(),
                git_ref: branch_name(build.source_branch.as_deref()),
                status,
                step,
                started,
                finished,
                interrupt: fresh && may_interrupt,
                url: build_url(&self.web, project, build.id),
            };
            let key = format!("{}:{project}:{pipeline}", self.id);
            if self.runs.get(&key).map(|(run, _)| run) != Some(&fact) {
                if finished.is_some() {
                    info!(source = %self.id, %project, %pipeline, ?status, "build finished");
                }
                self.runs.insert(key.clone(), (fact.clone(), build.id));
                out.push(Update::Run { key, run: fact });
            }
        }

        // Remember builds in progress; forget the ones that finished or
        // dropped off the list, taking their job screen down.
        let in_progress: HashSet<u64> = builds
            .iter()
            .filter(|b| build_status(b) == RunStatus::Running)
            .map(|b| b.id)
            .collect();
        for build in builds.iter().filter(|b| in_progress.contains(&b.id)) {
            let pipeline = build.definition.name.clone();
            let live_key = (project.to_string(), build.id);
            let kind = self.kind_of(&pipeline);
            let url = build_url(&self.web, project, build.id);
            self.live.entry(live_key).or_insert_with(|| LiveBuild {
                run_key: format!("{}:{project}:{pipeline}", self.id),
                git_ref: branch_name(build.source_branch.as_deref()),
                commit: build
                    .source_version
                    .as_deref()
                    .unwrap_or("")
                    .chars()
                    .take(7)
                    .collect(),
                started: unix_seconds(build.start_time.as_deref())
                    .or_else(|| unix_seconds(build.queue_time.as_deref()))
                    .unwrap_or(now),
                kind,
                pipeline,
                interrupt: may_interrupt,
                waiting: false,
                check_polls: 0,
                job_shown: false,
                url,
            });
        }
        let over: Vec<(String, u64)> = self
            .live
            .keys()
            .filter(|(p, id)| p == project && !in_progress.contains(id))
            .cloned()
            .collect();
        for live_key in over {
            if let Some(live) = self.live.remove(&live_key)
                && live.job_shown
            {
                out.push(Update::JobDone {
                    key: self.job_key(project, live_key.1),
                });
            }
        }

        // Forget failing steps of builds no longer listed.
        let listed: HashSet<u64> = builds.iter().map(|b| b.id).collect();
        self.failed_steps
            .retain(|(p, id), _| p != project || listed.contains(id));
        out
    }

    /// The timeline of a build in progress, from a poll. Returns the updates
    /// and whether every stage has finished (the build result is then worth
    /// fetching).
    pub fn on_timeline(
        &mut self,
        project: &str,
        build_id: u64,
        records: &[Record],
        now: u64,
    ) -> (Vec<Update>, bool) {
        let mut out = Vec::new();
        let view = timeline::analyse(records);
        let job_key = self.job_key(project, build_id);
        let project_label = source::label(&self.alias, project);
        // A late answer for a build that already finished changes nothing.
        let Some(live) = self.live.get_mut(&(project.to_string(), build_id)) else {
            return (out, false);
        };

        let was_waiting = live.waiting;
        live.check_polls = if view.waiting_check && !view.waiting_approval {
            live.check_polls.saturating_add(1)
        } else {
            0
        };
        live.waiting = view.waiting_approval || live.check_polls >= CHECK_POLLS;
        if live.waiting != was_waiting {
            let status = if live.waiting {
                RunStatus::Waiting
            } else {
                RunStatus::Running
            };
            if live.waiting {
                info!(source = %self.id, %project, pipeline = %live.pipeline, approval = view.waiting_approval, "waiting");
                out.push(Update::Notice {
                    notice: NoticeData {
                        text: if view.waiting_approval {
                            "Approval needed"
                        } else {
                            "Check pending"
                        }
                        .into(),
                        sub: truncate(
                            &format!("{project_label} {}", live.pipeline),
                            MAX_TITLE_CHARS,
                        ),
                        source: KIND.into(),
                    },
                });
            }
            // Show the change on the pipelines page now, if this build is
            // the one the page lists.
            if let Some((run, run_build)) = self.runs.get_mut(&live.run_key)
                && *run_build == build_id
            {
                run.status = status;
                out.push(Update::Run {
                    key: live.run_key.clone(),
                    run: run.clone(),
                });
            }
        }

        if live.waiting {
            // A deploy that waits for a person must not hold the screen.
            if live.job_shown {
                live.job_shown = false;
                out.push(Update::JobDone { key: job_key });
            }
        } else {
            // Quiet projects get a job too (for the kiosk list); the flag
            // keeps it off the ESP screen.
            let data = job_data(live, &project_label, &view);
            debug!(key = %job_key, step = ?data.step, progress = ?data.progress, "build running");
            live.job_shown = true;
            out.push(Update::JobRunning {
                key: job_key,
                job: RunningJob {
                    data,
                    interrupt: live.interrupt,
                    updated: now,
                    url: live.url.clone(),
                },
            });
        }
        (out, view.finished)
    }

    /// Deploy if the pipeline name has one of the `deploy_words`.
    fn kind_of(&self, pipeline: &str) -> JobKind {
        let name = pipeline.to_lowercase();
        if self.deploy_words.iter().any(|w| name.contains(w.as_str())) {
            JobKind::Deploy
        } else {
            JobKind::Build
        }
    }

    /// Key for a build's job screen in the fact store.
    fn job_key(&self, project: &str, build_id: u64) -> String {
        format!("{}:{project}:job:{build_id}", self.id)
    }
}

fn job_data(live: &LiveBuild, project_label: &str, view: &View) -> JobData {
    JobData {
        source: KIND.into(),
        project: truncate(project_label, MAX_TITLE_CHARS),
        kind: live.kind,
        pipeline: truncate(&live.pipeline, MAX_TITLE_CHARS),
        git_ref: truncate(&live.git_ref, MAX_TITLE_CHARS),
        commit: live.commit.clone(),
        step: view.step.as_deref().map(|s| truncate(s, MAX_TITLE_CHARS)),
        step_no: view.step_no,
        step_count: view.step_count,
        progress: view.fraction,
        started: live.started,
        others: 0,
    }
}

/// Web page of a build: `{site}/{org}/{project}/_build/results?buildId=N`.
fn build_url(web: &(String, String), project: &str, build_id: u64) -> Option<String> {
    let (base, org) = web;
    source::web_url(
        base,
        &[org, project, "_build", "results"],
        &[("buildId", &build_id.to_string())],
    )
}

/// Web page of a pull request: `{site}/{org}/{project}/_git/{repo}/pullrequest/N`.
fn pull_url(web: &(String, String), project: &str, repo: &str, number: u64) -> Option<String> {
    let (base, org) = web;
    source::web_url(
        base,
        &[
            org,
            project,
            "_git",
            repo,
            "pullrequest",
            &number.to_string(),
        ],
        &[],
    )
}

fn pulls_key(id: &SourceId, project: &str, repo: &str) -> String {
    format!("{id}:{project}/{repo}")
}

fn open_pull(pull: &PullRequest, now: u64, url: Option<String>) -> OpenPull {
    OpenPull {
        number: pull.pull_request_id,
        title: pull.title.clone(),
        created: unix_seconds(pull.creation_date.as_deref()).unwrap_or(now),
        url,
    }
}

/// The newest build of each pipeline, in list order (the API lists newest
/// queued first).
fn latest_per_pipeline(builds: &[Build]) -> Vec<&Build> {
    let mut seen = BTreeSet::new();
    builds
        .iter()
        .filter(|b| seen.insert(b.definition.name.as_str()))
        .collect()
}

/// `refs/heads/main` becomes `main`, `refs/pull/12/merge` becomes `PR 12`.
fn branch_name(source_branch: Option<&str>) -> String {
    let full = source_branch.unwrap_or("");
    if let Some(rest) = full.strip_prefix("refs/pull/") {
        return format!("PR {}", rest.split('/').next().unwrap_or(rest));
    }
    full.strip_prefix("refs/heads/")
        .or_else(|| full.strip_prefix("refs/tags/"))
        .unwrap_or(full)
        .to_string()
}

/// Map an Azure DevOps build to the shared run status (plan 8.1). A partly
/// succeeded build counts as failed; cancelled builds never raise an alert.
/// "Waiting for approval" is decided from the timeline, not here.
fn build_status(build: &Build) -> RunStatus {
    match build.status.as_deref().unwrap_or("") {
        "completed" => match build.result.as_deref() {
            Some("succeeded") => RunStatus::Success,
            Some("failed" | "partiallySucceeded") => RunStatus::Failed,
            _ => RunStatus::Neutral,
        },
        "inProgress" | "cancelling" => RunStatus::Running,
        // notStarted, postponed, none
        _ => RunStatus::Queued,
    }
}

#[cfg(test)]
mod tests;
