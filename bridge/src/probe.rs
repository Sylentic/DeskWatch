//! Connection tests for the setup page: "can the bridge reach this source
//! with this token, and see what it is meant to watch?"
//!
//! A test makes a few cheap, read-only requests (one per repository, project
//! or host, plus one per optional feature such as runners) and reports each as
//! a check with a plain message. It uses the same credentials and the same
//! client code as the real source, so a pass here means the source will work.
//! Messages never contain a credential: tokens only travel in headers, and
//! errors are shortened and reported as text.
//!
//! Nothing is changed anywhere. At most `MAX_TARGETS` repositories, projects
//! or hosts are checked per test, so a test stays quick.

use serde::Serialize;

use crate::config::{
    AzureDevopsAuth, AzureDevopsConfig, BitbucketConfig, Config, GiteaConfig, GithubAuth,
    GithubConfig, GitlabConfig, MqttConfig, PrometheusConfig,
};
use crate::gitea::api::{GiteaApi, HttpApi as GiteaHttp};
use crate::source::{Health, Secret, load_secret};

/// Most repositories, projects or hosts checked in one test.
const MAX_TARGETS: usize = 10;

/// Longest error text kept in a message.
const MAX_MESSAGE: usize = 300;

/// One thing that was tried.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub message: String,
}

/// The result of one test: every check, in the order tried.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct Report {
    pub ok: bool,
    pub checks: Vec<Check>,
}

impl Report {
    fn pass(&mut self, name: impl Into<String>, message: impl Into<String>) {
        self.checks.push(Check {
            name: name.into(),
            ok: true,
            message: message.into(),
        });
    }

    fn fail(&mut self, name: impl Into<String>, message: impl Into<String>) {
        self.checks.push(Check {
            name: name.into(),
            ok: false,
            message: message.into(),
        });
    }

    /// Record the outcome of a request. `hint` says what to look at when the
    /// server refuses the token (a missing scope, usually).
    fn result<T>(&mut self, name: impl Into<String>, outcome: anyhow::Result<T>, hint: &str) {
        match outcome {
            Ok(_) => self.pass(name, "ok"),
            Err(err) => self.fail(name, explain(&err, hint)),
        }
    }

    fn done(mut self) -> Self {
        self.ok = !self.checks.is_empty() && self.checks.iter().all(|c| c.ok);
        self
    }
}

/// A request error in words a person can act on.
fn explain(err: &anyhow::Error, hint: &str) -> String {
    let status = err
        .chain()
        .find_map(|e| e.downcast_ref::<reqwest::Error>())
        .and_then(reqwest::Error::status);
    let text = match (Health::from_error(err), status) {
        (Health::AuthFailed, Some(status)) => {
            format!("refused by the server (HTTP {status}). {hint}")
        }
        (Health::AuthFailed, None) => format!("the server asked for a sign-in. {hint}"),
        (_, Some(status)) if status == reqwest::StatusCode::NOT_FOUND => {
            // The URL carries no credential (tokens travel in headers). It
            // shows which name or path the server did not know, with any
            // special characters as they were sent.
            let url = err
                .chain()
                .find_map(|e| e.downcast_ref::<reqwest::Error>())
                .and_then(reqwest::Error::url)
                .map(|u| format!(" for {}", u.path()))
                .unwrap_or_default();
            format!("not found (HTTP 404){url}, or not visible to this token")
        }
        _ => chain_text(err),
    };
    let text = text.trim().to_string();
    match text.char_indices().nth(MAX_MESSAGE) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text,
    }
}

/// `err` and its causes joined with ": ", leaving out a cause that the text
/// before it already ends with (I/O errors often repeat it).
fn chain_text(err: &anyhow::Error) -> String {
    let mut text = String::new();
    for cause in err.chain() {
        let part = cause.to_string();
        if text.ends_with(&part) {
            continue;
        }
        if !text.is_empty() {
            text.push_str(": ");
        }
        text.push_str(&part);
    }
    text
}

/// Secrets that are not on disk yet: a token being entered on the setup page
/// is tested before it is saved. A name found here wins over the files.
pub type Overrides = std::collections::HashMap<String, Secret>;

fn lookup(overrides: &Overrides, name: &str) -> anyhow::Result<Secret> {
    match overrides.get(name) {
        Some(secret) => Ok(secret.clone()),
        None => load_secret(name),
    }
}

/// Look up a credential, as the real source would.
fn credential(
    report: &mut Report,
    overrides: &Overrides,
    label: &str,
    name: &str,
) -> Option<Secret> {
    match lookup(overrides, name) {
        Ok(secret) => Some(secret),
        Err(err) => {
            report.fail(label, chain_text(&err));
            None
        }
    }
}

/// Test the source with this id (`github:main`, `gitea:home`, ...), or
/// `None` when the config has no such source.
pub async fn source(config: &Config, id: &str) -> Option<Report> {
    source_with(config, id, &Overrides::new()).await
}

/// Like `source`, with secrets that are not saved yet.
pub async fn source_with(config: &Config, id: &str, overrides: &Overrides) -> Option<Report> {
    let (kind, name) = id.split_once(':')?;
    let s = &config.source;
    let report = match kind {
        "github" => github(s.github.iter().find(|c| c.name == name)?, overrides).await,
        "bitbucket" => bitbucket(s.bitbucket.iter().find(|c| c.name == name)?, overrides).await,
        "gitlab" => gitlab(s.gitlab.iter().find(|c| c.name == name)?, overrides).await,
        "gitea" => gitea(s.gitea.iter().find(|c| c.name == name)?, overrides).await,
        "azure_devops" => {
            azure_devops(s.azure_devops.iter().find(|c| c.name == name)?, overrides).await
        }
        "prometheus" => prometheus(s.prometheus.iter().find(|c| c.name == name)?, overrides).await,
        _ => return None,
    };
    Some(report.done())
}

async fn github(config: &GithubConfig, overrides: &Overrides) -> Report {
    let mut report = Report::default();
    let api = match config.auth {
        GithubAuth::Token => {
            let Some(token) = credential(&mut report, overrides, "token", &config.token_file)
            else {
                return report;
            };
            crate::github::api::HttpApi::new(&config.base_url, token)
        }
        GithubAuth::App => {
            let Some(key) = credential(
                &mut report,
                overrides,
                "private key",
                &config.private_key_file,
            ) else {
                return report;
            };
            crate::github::app::GithubApp::new(&config.app_id, &config.installation_id, &key)
                .and_then(|app| crate::github::api::HttpApi::with_app(&config.base_url, app))
        }
    };
    let api = match api {
        Ok(api) => api,
        Err(err) => {
            report.fail("setup", format!("{err:#}"));
            return report;
        }
    };
    let hint = match config.auth {
        GithubAuth::Token => {
            "Check that the token is valid and that it may read this repository \
             (fine-grained token: Contents or Metadata, Pull requests and Actions, all read-only)."
        }
        GithubAuth::App => {
            "Check the App id, the installation id and the private key, and that the App is \
             installed on this repository (Metadata, Pull requests and Actions: read)."
        }
    };
    for repo in config.repos.iter().take(MAX_TARGETS) {
        report.result(format!("repository {repo}"), api.repo(repo).await, hint);
    }
    match config.runner_scopes() {
        Ok(scopes) => {
            for scope in scopes.iter().take(MAX_TARGETS) {
                report.result(
                    format!("runners {scope}"),
                    api.runners(scope).await,
                    "Runners need more rights: repository runners \"Administration: read\", \
                     organisation runners \"Self-hosted runners: read\".",
                );
            }
        }
        Err(err) => report.fail("runners", format!("{err:#}")),
    }
    report
}

async fn bitbucket(config: &BitbucketConfig, overrides: &Overrides) -> Report {
    let mut report = Report::default();
    let Some(token) = credential(&mut report, overrides, "token", &config.token_file) else {
        return report;
    };
    let api = match crate::bitbucket::api::HttpApi::new(&config.base_url, token) {
        Ok(api) => api,
        Err(err) => {
            report.fail("setup", format!("{err:#}"));
            return report;
        }
    };
    let hint = "Check that the access token is valid (a repository, project or workspace access token, \
                with Pull requests: Read and Pipelines: Read) and that it covers this repository. \
                A repository that does not exist answers the same way.";
    for repo in config.repos.iter().take(MAX_TARGETS) {
        report.result(
            format!("repository {repo}"),
            api.repository(repo).await,
            hint,
        );
    }
    match config.runner_scopes() {
        Ok(scopes) => {
            for scope in scopes.iter().take(MAX_TARGETS) {
                report.result(
                    format!("runners {scope}"),
                    api.runners(scope).await.map(|_| ()),
                    "Runners need the Runners: Read right on the token.",
                );
            }
        }
        Err(err) => report.fail("runners", format!("{err:#}")),
    }
    report
}

async fn gitlab(config: &GitlabConfig, overrides: &Overrides) -> Report {
    let mut report = Report::default();
    let Some(token) = credential(&mut report, overrides, "token", &config.token_file) else {
        return report;
    };
    let api = match crate::gitlab::api::HttpApi::new(&config.base_url, token) {
        Ok(api) => api,
        Err(err) => {
            report.fail("setup", format!("{err:#}"));
            return report;
        }
    };
    let hint = "Check that the token is valid (scope read_api) and that its user may see this \
                project (Reporter role or higher). A project that does not exist answers the same way.";
    for project in config.projects.iter().take(MAX_TARGETS) {
        report.result(
            format!("project {project}"),
            api.project(project).await,
            hint,
        );
    }
    match config.runner_scopes() {
        Ok(scopes) => {
            for scope in scopes.iter().take(MAX_TARGETS) {
                report.result(
                    format!("runners {scope}"),
                    api.runners(scope).await.map(|_| ()),
                    "Runners need more rights: the Maintainer role on a project, Owner on a \
                     group, an administrator token for \"instance\".",
                );
            }
        }
        Err(err) => report.fail("runners", format!("{err:#}")),
    }
    report
}

async fn gitea(config: &GiteaConfig, overrides: &Overrides) -> Report {
    let mut report = Report::default();
    match credential_present(overrides, &config.webhook_secret_file) {
        Ok(()) => report.pass(
            "webhook secret",
            "found (its value cannot be checked from here; it must match the webhook in Gitea)",
        ),
        Err(err) => report.fail("webhook secret", err),
    }
    let token = match &config.token_file {
        Some(file) => match credential(&mut report, overrides, "token", file) {
            Some(token) => Some(token),
            None => return report,
        },
        None => None,
    };
    let has_token = token.is_some();
    let api = match GiteaHttp::new(&config.base_url, token) {
        Ok(api) => api,
        Err(err) => {
            report.fail("setup", format!("{err:#}"));
            return report;
        }
    };
    report.result("server", api.version().await, "");
    if has_token {
        report.result(
            "token",
            api.user().await,
            "Check that the token is valid and has the read:repository scope.",
        );
    }
    for repo in config.repos.iter().take(MAX_TARGETS) {
        report.result(
            format!("repository {repo}"),
            api.repo(repo).await,
            "A private repository needs a token_file with read:repository.",
        );
    }
    match config.runner_scopes() {
        Ok(scopes) => {
            for scope in scopes.iter().take(MAX_TARGETS) {
                report.result(
                    format!("runners {scope}"),
                    api.runners(scope).await,
                    "Runners need a token with the right to list them (admin for \"admin\").",
                );
            }
        }
        Err(err) => report.fail("runners", format!("{err:#}")),
    }
    report
}

async fn azure_devops(config: &AzureDevopsConfig, overrides: &Overrides) -> Report {
    let mut report = Report::default();
    let api = match config.auth {
        AzureDevopsAuth::Pat => {
            let Some(token) = credential(&mut report, overrides, "token", &config.token_file)
            else {
                return report;
            };
            crate::azure_devops::api::HttpApi::new(&config.base_url, &config.organization, token)
        }
        AzureDevopsAuth::Entra => {
            let Some(client_secret) = credential(
                &mut report,
                overrides,
                "client secret",
                &config.client_secret_file,
            ) else {
                return report;
            };
            crate::azure_devops::api::HttpApi::with_entra(
                &config.base_url,
                &config.organization,
                crate::azure_devops::api::EntraApp {
                    login_url: config.login_url.clone(),
                    tenant_id: config.tenant_id.clone(),
                    client_id: config.client_id.clone(),
                    client_secret,
                },
            )
        }
    };
    let api = match api {
        Ok(api) => api,
        Err(err) => {
            report.fail("setup", format!("{err:#}"));
            return report;
        }
    };
    let hint = match config.auth {
        AzureDevopsAuth::Pat => {
            "Check that the PAT is valid, not expired, and covers this organisation (Build: read)."
        }
        AzureDevopsAuth::Entra => {
            "Check the tenant id, the application id and the client secret, and that the app \
             was added to the organisation as a user (Build: read)."
        }
    };
    for project in config.projects.iter().take(MAX_TARGETS) {
        let builds = api.builds(project).await;
        let missing = is_not_found(&builds);
        let name = format!("project {project}: builds");
        match builds {
            // A project the server does not know: say which projects this
            // token does see, because the name is the usual mistake.
            Err(err) if missing => {
                let mut message = explain(&err, hint);
                message.push_str(&visible_projects(&api).await);
                report.fail(name, message);
            }
            other => report.result(name, other, hint),
        }
        if config.pull_requests {
            report.result(
                format!("project {project}: pull requests"),
                api.pulls(project).await,
                "Pull requests need the Code (Read) scope.",
            );
        }
    }
    if !config.agent_pools.is_empty() {
        match api.pools().await {
            Ok(pools) => {
                for wanted in config.agent_pools.iter().take(MAX_TARGETS) {
                    if pools.iter().any(|p| p.name.eq_ignore_ascii_case(wanted)) {
                        report.pass(format!("agent pool {wanted}"), "ok");
                    } else {
                        report.fail(format!("agent pool {wanted}"), "no pool with this name");
                    }
                }
            }
            Err(err) => report.fail(
                "agent pools",
                explain(&err, "Agent pools need the Agent Pools (Read) scope."),
            ),
        }
    }
    report
}

fn is_not_found<T>(outcome: &anyhow::Result<T>) -> bool {
    outcome.as_ref().err().is_some_and(|err| {
        err.chain()
            .find_map(|e| e.downcast_ref::<reqwest::Error>())
            .and_then(reqwest::Error::status)
            == Some(reqwest::StatusCode::NOT_FOUND)
    })
}

/// " Projects this token can see: A, B." for a failed project check, or a hint
/// about the organisation name when the list cannot be read either.
async fn visible_projects(api: &crate::azure_devops::api::HttpApi) -> String {
    match api.projects().await {
        Ok(names) if names.is_empty() => {
            " This token sees no projects in the organisation: check the organization name \
             and that the PAT was made for it."
                .to_string()
        }
        Ok(names) => {
            let shown: Vec<_> = names.iter().take(MAX_TARGETS).map(String::as_str).collect();
            format!(" Projects this token can see: {}.", shown.join(", "))
        }
        // The organisation itself is not found or not readable with this
        // token: then the name before the project is the likelier mistake.
        Err(_) => " The list of projects could not be read either: check the organization name, \
                    and that the PAT was made for that organisation."
            .to_string(),
    }
}

async fn prometheus(config: &PrometheusConfig, overrides: &Overrides) -> Report {
    let mut report = Report::default();
    let token = match &config.token_file {
        Some(file) => match credential(&mut report, overrides, "token", file) {
            Some(token) => Some(token),
            None => return report,
        },
        None => None,
    };
    let api = match crate::prometheus::api::HttpApi::new(&config.url, token) {
        Ok(api) => api,
        Err(err) => {
            report.fail("setup", format!("{err:#}"));
            return report;
        }
    };
    report.result(
        "server",
        api.query("up").await,
        "Check the token, or the proxy in front of Prometheus.",
    );
    for host in config.hosts.iter().take(MAX_TARGETS) {
        let query = format!("up{{instance=\"{}\"}}", escape_label(&host.instance));
        match api.query(&query).await {
            Ok(samples) if !samples.is_empty() => {
                report.pass(format!("host {}", host.name), "found in Prometheus");
            }
            Ok(_) => report.fail(
                format!("host {}", host.name),
                format!(
                    "Prometheus has no target with instance {:?}; copy it from the Targets page",
                    host.instance
                ),
            ),
            Err(err) => report.fail(format!("host {}", host.name), explain(&err, "")),
        }
    }
    report
}

/// A PromQL label value: backslash and quote escaped, newlines removed.
fn escape_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace(['\n', '\r'], " ")
}

/// Is the credential there? Without reading its value into the report.
fn credential_present(overrides: &Overrides, name: &str) -> Result<(), String> {
    lookup(overrides, name)
        .map(|_| ())
        .map_err(|e| chain_text(&e))
}

/// Test the broker connection and login.
pub async fn mqtt(config: &MqttConfig) -> Report {
    mqtt_with(config, &Overrides::new()).await
}

/// `mqtt`, with a password typed in the form but not saved yet taking the
/// place of the one in the credential file it will be saved to.
pub async fn mqtt_with(config: &MqttConfig, overrides: &Overrides) -> Report {
    let mut report = Report::default();
    if !config.enabled {
        report.fail("broker", "MQTT is switched off in the config");
        return report.done();
    }
    let typed = config
        .password_file
        .as_deref()
        .and_then(|name| overrides.get(name))
        .cloned();
    let password = match typed.map(Ok).unwrap_or_else(|| {
        crate::mqtt::password(config).map(|p| p.unwrap_or_else(|| Secret::new("")))
    }) {
        Ok(password) if password.expose().is_empty() => None,
        Ok(password) => Some(password),
        Err(err) => {
            report.fail("password", format!("{err:#}"));
            return report.done();
        }
    };
    let target = format!("{}:{}", config.host, config.port);
    match crate::mqtt::probe(config, password).await {
        Ok(()) => report.pass(
            format!("connect to {target}"),
            "connected and logged in (what the login may publish is not checked)",
        ),
        Err(err) => report.fail(
            format!("connect to {target}"),
            explain(
                &err,
                "Check host, port, TLS and the login (docs/mosquitto.md).",
            ),
        ),
    }
    report.done()
}

#[cfg(test)]
pub(crate) mod tests;
