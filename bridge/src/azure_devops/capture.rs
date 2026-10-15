//! `deskwatch-bridge ado-capture`: records what a real Azure DevOps
//! organisation answers to the requests the bridge sends, with every name
//! replaced (see `sanitise.rs`), so the answers can be shared and compared
//! with the hand-written test data in `testdata/`.
//!
//! Only `GET` requests, the same ones as the source. The token is read from a
//! file or the `AZURE_DEVOPS_PAT` variable, never printed and never written.
//! Before the result is saved it is checked once more for the organisation
//! name, the project names, extra words from `--hide`, the token, mail
//! addresses and links; one hit and nothing is written.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

use super::api::{AZURE_DEVOPS_COM, HttpApi, plain_name};
use super::sanitise::Sanitiser;
use crate::source::{AuthRefused, Secret};

const USAGE: &str = "\
usage: deskwatch-bridge ado-capture --organization ORG --project NAME [options]

Reads the pipelines, pull requests and agent pools of an Azure DevOps
organisation, hides every name in them and writes one file you can share.

  --organization ORG   the organisation (the part after dev.azure.com/)
  --project NAME       a project; repeat for more than one
  --token-file PATH    file holding the token (or set AZURE_DEVOPS_PAT)
  --address URL        default https://dev.azure.com
  --pool NAME          only this self-hosted pool; repeat. Default: the first 5
  --timelines N        timelines of the N newest builds per project (default 8)
  --build ID           also the timeline of this build (a run waiting on an
                       approval or check); repeat
  --hide WORD          a word that must not occur in the result; repeat
  --out FILE           default ado-capture.json

The token is not printed or saved. See docs/azure-devops-verify.md.";

/// What to capture.
#[derive(Debug, Default)]
pub struct Options {
    pub organization: String,
    pub projects: Vec<String>,
    pub pools: Vec<String>,
    pub timelines: usize,
    pub builds: Vec<u64>,
    pub hide: Vec<String>,
}

/// At most this many self-hosted pools when `--pool` is not given.
const MAX_POOLS: usize = 5;

/// Command line entry: `args` is everything after `ado-capture`.
pub async fn run_cli(args: Vec<String>) -> Result<()> {
    let mut opts = Options {
        timelines: 8,
        ..Options::default()
    };
    let mut address = AZURE_DEVOPS_COM.to_string();
    let mut token_file: Option<PathBuf> = None;
    let mut out = PathBuf::from("ado-capture.json");

    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if matches!(arg.as_str(), "-h" | "--help") {
            println!("{USAGE}");
            return Ok(());
        }
        let mut value = || {
            args.next()
                .with_context(|| format!("{arg} needs a value\n{USAGE}"))
        };
        match arg.as_str() {
            "--organization" => opts.organization = value()?,
            "--project" => opts.projects.push(value()?),
            "--token-file" => token_file = Some(value()?.into()),
            "--address" => address = value()?,
            "--pool" => opts.pools.push(value()?),
            "--timelines" => {
                opts.timelines = value()?.parse().context("--timelines is a number")?
            }
            "--build" => opts
                .builds
                .push(value()?.parse().context("--build is a build id")?),
            "--hide" => opts.hide.push(value()?),
            "--out" => out = value()?.into(),
            other => bail!("unknown option {other}\n{USAGE}"),
        }
    }
    if opts.organization.trim().is_empty() || opts.projects.is_empty() {
        bail!("--organization and at least one --project are required\n{USAGE}");
    }

    let token = match token_file {
        Some(path) => std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read token file {}", path.display()))?,
        None => std::env::var("AZURE_DEVOPS_PAT")
            .context("no token: use --token-file or set AZURE_DEVOPS_PAT")?,
    };
    let token = token.trim().to_string();
    if token.is_empty() {
        bail!("the token is empty");
    }

    let api = HttpApi::new(&address, &opts.organization, Secret::new(token.clone()))?;
    let result = capture(&api, &opts, &token).await?;
    let text = serde_json::to_string_pretty(&result)?;
    std::fs::write(&out, text + "\n").with_context(|| format!("cannot write {}", out.display()))?;

    println!("Written: {}", out.display());
    println!("{}", summary(&result["report"]));
    println!(
        "Look through the file before you share it. Names are replaced, but you are the judge."
    );
    Ok(())
}

/// Fetch, sanitise and check. The result holds a `report` (what worked, what
/// was seen) and the answers themselves.
pub async fn capture(api: &HttpApi, opts: &Options, token: &str) -> Result<Value> {
    let mut sanitiser = Sanitiser::new();
    let mut requests: Vec<Value> = Vec::new();
    let mut note = |what: &str, outcome: &Result<Value>| {
        requests.push(match outcome {
            Ok(_) => json!({"request": what, "ok": true}),
            Err(e) => json!({"request": what, "ok": false, "error": describe(e)}),
        });
    };
    let mut record_types: std::collections::BTreeMap<String, u64> = Default::default();
    let mut build_states: std::collections::BTreeMap<String, u64> = Default::default();

    let mut projects = Map::new();
    for project in &opts.projects {
        let name = plain_name(project);
        let key = sanitiser.name(&name, "project");
        let mut entry = Map::new();

        let builds = api
            .raw(
                &[&name, "_apis", "build", "builds"],
                &[("queryOrder", "queueTimeDescending"), ("$top", "50")],
            )
            .await;
        note(&format!("{key} builds"), &builds);
        let mut ids: Vec<u64> = Vec::new();
        if let Ok(builds) = builds {
            for b in builds["value"].as_array().into_iter().flatten() {
                let status = b["status"].as_str().unwrap_or("?");
                let result = b["result"].as_str().unwrap_or("-");
                *build_states
                    .entry(format!("{status}/{result}"))
                    .or_default() += 1;
            }
            ids = builds["value"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|b| b["id"].as_u64())
                .take(opts.timelines)
                .collect();
            entry.insert("builds".into(), sanitiser.value(builds, "build"));
        }
        for id in &opts.builds {
            if !ids.contains(id) {
                ids.push(*id);
            }
        }

        let mut timelines = Map::new();
        for id in ids {
            let text = id.to_string();
            let timeline = api
                .raw(&[&name, "_apis", "build", "builds", &text, "timeline"], &[])
                .await;
            let placeholder = sanitiser.id(id);
            note(&format!("{key} timeline {placeholder}"), &timeline);
            if let Ok(timeline) = timeline {
                for r in timeline["records"].as_array().into_iter().flatten() {
                    let kind = r["type"].as_str().unwrap_or("?");
                    let state = r["state"].as_str().unwrap_or("?");
                    *record_types.entry(format!("{kind} ({state})")).or_default() += 1;
                }
                timelines.insert(
                    placeholder.to_string(),
                    sanitiser.value(timeline, "timeline"),
                );
            }
        }
        entry.insert("timelines".into(), Value::Object(timelines));

        let pulls = api
            .raw(
                &[&name, "_apis", "git", "pullrequests"],
                &[("searchCriteria.status", "active"), ("$top", "100")],
            )
            .await;
        note(&format!("{key} pull requests"), &pulls);
        if let Ok(pulls) = pulls {
            entry.insert("pulls".into(), sanitiser.value(pulls, "pullrequest"));
        }
        projects.insert(key, Value::Object(entry));
    }

    let mut result = Map::new();
    let pools = api.raw(&["_apis", "distributedtask", "pools"], &[]).await;
    note("agent pools", &pools);
    let (mut agents, mut jobrequests) = (Map::new(), Map::new());
    if let Ok(pools) = pools {
        let mut chosen: Vec<u64> = Vec::new();
        for pool in pools["value"].as_array().into_iter().flatten() {
            let (Some(id), Some(name)) = (pool["id"].as_u64(), pool["name"].as_str()) else {
                continue;
            };
            let wanted = if opts.pools.is_empty() {
                pool["isHosted"] == Value::Bool(false) && chosen.len() < MAX_POOLS
            } else {
                opts.pools.iter().any(|p| p.eq_ignore_ascii_case(name))
            };
            if wanted {
                chosen.push(id);
            }
        }
        for id in chosen {
            let text = id.to_string();
            let placeholder = sanitiser.id(id);
            let found = api
                .raw(
                    &["_apis", "distributedtask", "pools", &text, "agents"],
                    &[("includeAssignedRequest", "true")],
                )
                .await;
            note(&format!("pool {placeholder} agents"), &found);
            if let Ok(found) = found {
                agents.insert(placeholder.to_string(), sanitiser.value(found, "agent"));
            }
            let found = api
                .raw(
                    &["_apis", "distributedtask", "pools", &text, "jobrequests"],
                    &[("completedRequestCount", "0")],
                )
                .await;
            note(&format!("pool {placeholder} job requests"), &found);
            if let Ok(found) = found {
                jobrequests.insert(placeholder.to_string(), sanitiser.value(found, "request"));
            }
        }
        result.insert("pools".into(), sanitiser.value(pools, "pool"));
    }
    result.insert("agents".into(), Value::Object(agents));
    result.insert("jobrequests".into(), Value::Object(jobrequests));
    result.insert("projects".into(), Value::Object(projects));

    let mut out = Map::new();
    out.insert("deskwatch_capture".into(), json!(1));
    out.insert(
        "report".into(),
        json!({
            "bridge_version": env!("CARGO_PKG_VERSION"),
            "api_version": "7.1",
            "requests": requests,
            "build_status_result": build_states,
            "timeline_record_type_state": record_types,
        }),
    );
    out.extend(result);
    let out = Value::Object(out);

    check_clean(&out, opts, token)?;
    Ok(out)
}

/// Plain words for the terminal: what worked, nothing from the data.
fn summary(report: &Value) -> String {
    let mut lines = Vec::new();
    for r in report["requests"].as_array().into_iter().flatten() {
        let request = r["request"].as_str().unwrap_or("?");
        match r["error"].as_str() {
            None => lines.push(format!("  ok       {request}")),
            Some(e) => lines.push(format!("  FAILED   {request}: {e}")),
        }
    }
    lines.join("\n")
}

/// Why a request failed, in words that hold no URL (a `reqwest` error holds
/// the whole address, with organisation and project in it).
fn describe(e: &anyhow::Error) -> String {
    if e.downcast_ref::<AuthRefused>().is_some() {
        "token refused".into()
    } else if let Some(re) = e.downcast_ref::<reqwest::Error>() {
        match re.status() {
            Some(s) => format!("HTTP {}", s.as_u16()),
            None if re.is_timeout() => "timeout".into(),
            None => "network error".into(),
        }
    } else {
        "unexpected answer".into()
    }
}

/// The last line of defence: refuse to hand out a result that still holds
/// something that should be hidden.
fn check_clean(result: &Value, opts: &Options, token: &str) -> Result<()> {
    let text = result.to_string().to_lowercase();
    let mut hidden: Vec<(String, String)> =
        vec![("the organisation name".into(), opts.organization.clone())];
    hidden.extend(
        opts.projects
            .iter()
            .map(|p| ("a project name".to_string(), plain_name(p))),
    );
    hidden.extend(
        opts.hide
            .iter()
            .map(|w| ("a --hide word".to_string(), w.clone())),
    );
    hidden.push(("the token".into(), token.to_string()));

    for (what, word) in hidden {
        let word = word.trim().to_lowercase();
        if word.is_empty() {
            continue;
        }
        let encoded = word.replace(' ', "%20");
        if text.contains(&word) || text.contains(&encoded) {
            bail!("the result still holds {what}; nothing was written");
        }
    }
    // Every address must be a placeholder.
    for (at, _) in text.match_indices('@') {
        if !text[at + 1..].starts_with("example.invalid") {
            bail!("the result still holds a mail address; nothing was written");
        }
    }
    for (at, _) in text.match_indices("://") {
        if !text[at..].starts_with("://example.invalid/") {
            bail!("the result still holds a link; nothing was written");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::api::tests::{Seen, api, mock_azure};
    use super::*;

    fn options() -> Options {
        Options {
            organization: "your-org".into(),
            projects: vec!["project-a".into()],
            timelines: 8,
            ..Options::default()
        }
    }

    #[tokio::test]
    async fn captures_every_request_kind_from_the_mock_without_names() {
        let seen = Arc::new(Seen::default());
        let base = mock_azure(seen.clone()).await;
        let api = api(&base);

        let out = capture(&api, &options(), "test-pat").await.unwrap();

        let text = out.to_string();
        for name in [
            "your-org",
            "project-a",
            "repo-a",
            "deploy-infra",
            "Homelab",
            "test-pat",
        ] {
            assert!(!text.contains(name), "{name} leaked");
        }
        assert!(
            out["report"]["requests"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["ok"] == true)
        );
        let project = &out["projects"]["project-1"];
        assert_eq!(project["builds"]["value"].as_array().unwrap().len(), 5);
        assert_eq!(project["pulls"]["value"].as_array().unwrap().len(), 3);
        assert_eq!(project["timelines"].as_object().unwrap().len(), 5);
        assert_eq!(out["pools"]["value"].as_array().unwrap().len(), 3);
        // Two self-hosted pools, each with agents and job requests.
        assert_eq!(out["agents"].as_object().unwrap().len(), 2);
        assert_eq!(out["jobrequests"].as_object().unwrap().len(), 2);
        // The facts the bridge decides on survive.
        assert_eq!(project["builds"]["value"][1]["status"], "inProgress");
        let types = &out["report"]["timeline_record_type_state"];
        assert!(
            types
                .as_object()
                .unwrap()
                .keys()
                .any(|k| k.starts_with("Stage")),
            "{types}"
        );
    }

    #[tokio::test]
    async fn a_failing_endpoint_is_reported_by_status_without_the_address() {
        let seen = Arc::new(Seen::default());
        let base = mock_azure(seen).await;
        // The mock only knows `your-org`: any other organisation answers 404.
        let api = HttpApi::new(&base, "wrong-org", Secret::new("test-pat")).unwrap();
        let opts = Options {
            organization: "wrong-org".into(),
            ..options()
        };
        let out = capture(&api, &opts, "test-pat").await.unwrap();
        let failed: Vec<_> = out["report"]["requests"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["ok"] == false)
            .collect();
        assert!(!failed.is_empty());
        assert_eq!(failed[0]["error"], "HTTP 404");
        assert!(!out.to_string().contains("wrong-org"));
    }

    #[test]
    fn the_final_check_refuses_a_leak() {
        let opts = Options {
            hide: vec!["Secret Thing".into()],
            ..options()
        };
        let clean = json!({"name": "project-1", "mail": "user-1@example.invalid"});
        assert!(check_clean(&clean, &opts, "tok-en-123").is_ok());
        for leak in [
            json!({"x": "my YOUR-ORG here"}),
            json!({"x": "the project-a one"}),
            json!({"x": "a secret%20thing"}),
            json!({"x": "pat tok-en-123"}),
            json!({"x": "jane@acme.example"}),
            json!({"x": "https://dev.azure.com/x"}),
        ] {
            assert!(check_clean(&leak, &opts, "tok-en-123").is_err(), "{leak}");
        }
    }
}
