use axum::body::Body;
use axum::http::Request;

use super::*;
use crate::hooks;
use crate::probe::tests::Temp;
use crate::source::SourceBody;

/// A config with one Gitea source named `name`, whose server does not exist.
/// `hook` names the webhook secret file, so a test can make it missing.
fn gitea(name: &str, temp: &Temp, hook: &str, repo: &str) -> Config {
    let secret = temp.token(hook, "hook-secret");
    let text = format!(
        "[[source.gitea]]\nname = {name:?}\nbase_url = \"http://127.0.0.1:9\"\n\
         webhook_secret_file = {secret}\nrepos = [{repo:?}]\n"
    );
    Config::from_toml(&text).unwrap()
}

fn supervisor() -> (Supervisor, mpsc::Receiver<SourceMsg>, Webhooks) {
    let (tx, rx) = mpsc::channel(64);
    let webhooks = Webhooks::default();
    (Supervisor::new(tx, webhooks.clone()), rx, webhooks)
}

/// The status a webhook path answers with an unsigned request: 401 when a
/// source listens there, 404 when nothing does.
async fn answer(webhooks: &Webhooks, path: &str) -> u16 {
    let request = Request::post(path).body(Body::empty()).unwrap();
    hooks::dispatch(axum::extract::State(webhooks.clone()), request)
        .await
        .status()
        .as_u16()
}

#[tokio::test]
async fn sources_start_stay_and_restart_only_when_needed() {
    let temp = Temp::new("sup-start");
    let (sup, _rx, webhooks) = supervisor();
    let config = gitea("home", &temp, "hook", "o/r");

    let first = sup.reconcile(&config, &[]).await;
    assert_eq!(first.started, ["gitea:home"]);
    assert!(sup.running().await.contains("gitea:home"));
    assert_eq!(answer(&webhooks, "/webhook/gitea/home").await, 401);

    // The same settings: nothing is touched.
    let again = sup.reconcile(&config, &[]).await;
    assert_eq!(again, Outcome::default());

    // Same settings but a replaced credential: restarted.
    let touched = sup.reconcile(&config, &["gitea:home".into()]).await;
    assert_eq!(touched.started, ["gitea:home"]);
    assert_eq!(answer(&webhooks, "/webhook/gitea/home").await, 401);

    // Changed settings: replaced, and the route is still there.
    let changed = gitea("home", &temp, "hook", "o/other");
    let replaced = sup.reconcile(&changed, &[]).await;
    assert_eq!(replaced.started, ["gitea:home"]);
    assert!(replaced.stopped.is_empty());
    assert_eq!(answer(&webhooks, "/webhook/gitea/home").await, 401);
}

#[tokio::test]
async fn a_removed_source_stops_loses_its_route_and_is_forgotten() {
    let temp = Temp::new("sup-remove");
    let (sup, mut rx, webhooks) = supervisor();
    sup.reconcile(&gitea("home", &temp, "hook", "o/r"), &[])
        .await;

    let gone = sup.reconcile(&Config::from_toml("").unwrap(), &[]).await;
    assert_eq!(gone.stopped, ["gitea:home"]);
    assert!(sup.running().await.is_empty());
    assert_eq!(answer(&webhooks, "/webhook/gitea/home").await, 404);

    // The main loop is told to drop what the source reported.
    let mut forgotten = false;
    while let Ok(msg) = rx.try_recv() {
        forgotten |=
            matches!(msg.body, SourceBody::Forget) && msg.source.to_string() == "gitea:home";
    }
    assert!(forgotten);
}

#[tokio::test]
async fn a_source_that_cannot_be_built_leaves_the_old_one_running() {
    let temp = Temp::new("sup-fail");
    let (sup, _rx, webhooks) = supervisor();
    sup.reconcile(&gitea("home", &temp, "hook", "o/r"), &[])
        .await;

    // The new settings name a webhook secret file that does not exist.
    let text = "[[source.gitea]]\nname = \"home\"\nbase_url = \"http://127.0.0.1:9\"\n\
                webhook_secret_file = \"/nonexistent/hook\"\nrepos = [\"o/new\"]\n";
    let outcome = sup.reconcile(&Config::from_toml(text).unwrap(), &[]).await;
    assert_eq!(outcome.failed.len(), 1);
    assert_eq!(outcome.failed[0].0, "gitea:home");
    assert!(outcome.started.is_empty() && outcome.stopped.is_empty());
    assert!(sup.running().await.contains("gitea:home"));
    assert_eq!(answer(&webhooks, "/webhook/gitea/home").await, 401);
}
