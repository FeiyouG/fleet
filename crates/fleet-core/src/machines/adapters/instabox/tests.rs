use super::mock::{serve, Mock};
use super::*;
use crate::machines::interfaces::MachineAdapter;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn adapter(m: &Mock, dir: &Path) -> InstaBox {
    let mut api = Api::new(&m.url, "FLEET_TEST_NO_SUCH_ENV");
    api.key = Some("ibk_test".into());
    api.poll = Duration::from_millis(5);
    api.op_timeout = Duration::from_secs(5);
    api.retry_cap = Duration::from_millis(5);
    InstaBox {
        api,
        keys_dir: dir.to_path_buf(),
        mosh: false,
    }
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("fleet-ibx-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn machine(id: &str, name: &str, state: &str) -> Value {
    json!({
        "id": id, "name": name, "state": state, "spec": {"vcpus": 4, "memMiB": 8192},
        "autoSuspendAfterSeconds": 1800, "maxRunMinutes": -1, "metadata": {"fleet.profile": "ibx"},
        "cooldownUntil": null, "createdAt": "2026-11-09T05:00:00Z"
    })
}

fn argv(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

#[test]
fn maps_states() {
    let r = to_row(&machine("box_1", "ibx-a", "running"));
    assert_eq!(r.state, MachineState::Running);
    assert_eq!(r.size.unwrap().label(), "4/8");
    assert_eq!(r.idle_after, Some(Duration::from_secs(1800)));
    assert_eq!(r.max_run, None);
    assert_eq!(r.profile(), Some("ibx"));

    let mut idle = machine("box_1", "a", "suspended");
    idle["autoSuspendedAt"] = json!("2026-11-09T06:00:00Z");
    assert_eq!(
        to_row(&idle).state,
        MachineState::Paused { by: PausedBy::Idle }
    );
    assert_eq!(
        to_row(&machine("b", "a", "suspended")).state,
        MachineState::Paused { by: PausedBy::User }
    );

    let mut cool = machine("b", "a", "suspended");
    cool["cooldownUntil"] = json!((Utc::now() + chrono::Duration::minutes(5)).to_rfc3339());
    assert!(matches!(to_row(&cool).state, MachineState::Cooldown { .. }));

    assert_eq!(
        to_row(&machine("b", "a", "stopped")).state,
        MachineState::Stopped
    );
    assert_eq!(
        to_row(&machine("b", "a", "resuming")).state,
        MachineState::Starting
    );
    let mut err = machine("b", "a", "error");
    err["lastError"] = json!({"code": "host_oom", "message": "host ran out of memory"});
    assert_eq!(
        to_row(&err).state,
        MachineState::Error {
            reason: "host ran out of memory".into()
        }
    );
}

#[tokio::test]
async fn create_sends_profile_and_waits_for_the_operation() {
    let m = serve(|r| match (r.method.as_str(), r.path.as_str()) {
        ("POST", "/v1/machines") => (
            202,
            json!({"id": "op_1", "machineId": "box_1", "type": "create", "state": "queued"}),
        ),
        ("GET", "/v1/operations/op_1") => (
            200,
            json!({"id": "op_1", "machineId": "box_1", "type": "create", "state": "succeeded"}),
        ),
        ("GET", "/v1/machines/box_1") => (200, machine("box_1", "ibx-fix-a", "running")),
        _ => (
            404,
            json!({"error": {"code": "not_found", "message": "no"}}),
        ),
    })
    .await;
    let dir = tmp("create");
    let ib = adapter(&m, &dir);
    let spec = MachineSpec {
        name: "ibx-fix-a".into(),
        profile: "ibx".into(),
        snapshot: Some("fleet-monid-services".into()),
        template: Some("agent-browser".into()),
        size: Some(Size {
            vcpus: 4,
            mem_mib: 8192,
        }),
        secrets: vec!["GITLAB_TOKEN".into()],
        files: [(
            ".local/share/opencode/auth.json".to_string(),
            "OPENCODE_AUTH".to_string(),
        )]
        .into(),
        ..MachineSpec::default()
    };
    let row = ib.create(&spec).await.unwrap();
    assert_eq!(row.id, "box_1");
    assert!(row.state.is_running());

    let post = &m.find("POST", "/v1/machines")[0];
    assert_eq!(post.headers["x-api-key"], "ibk_test");
    assert!(!post.headers["idempotency-key"].is_empty());
    let b = &post.body;
    assert_eq!(b["name"], "ibx-fix-a");
    assert_eq!(b["snapshotId"], "fleet-monid-services");
    assert!(b.get("templateId").is_none(), "snapshot wins over template");
    assert_eq!(b["vcpus"], 4);
    assert_eq!(b["memMiB"], 8192);
    assert_eq!(b["secrets"], json!(["GITLAB_TOKEN"]));
    assert_eq!(
        b["directSecrets"],
        json!([{"secret": "OPENCODE_AUTH", "file": ".local/share/opencode/auth.json"}])
    );
    assert_eq!(b["metadata"]["fleet.profile"], "ibx");
    assert!(
        b.get("autoSuspendAfterSeconds").is_none(),
        "unset settings are not sent"
    );
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn failed_operation_is_an_error() {
    let m = serve(|r| match r.path.as_str() {
        "/v1/machines" => (
            202,
            json!({"id": "op_1", "machineId": "box_1", "state": "failed", "type": "create",
                   "error": {"code": "no_capacity", "message": "no host has room"}}),
        ),
        _ => (404, json!({})),
    })
    .await;
    let dir = tmp("fail");
    let e = adapter(&m, &dir)
        .create(&MachineSpec {
            name: "x".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(e.to_string().contains("no host has room"), "{e}");
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn exec_maps_result_and_resolves_names() {
    let m = serve(|r| match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/v1/machines/ibx-fix-a") => {
            (404, json!({"error": {"code": "not_found", "message": "no"}}))
        }
        ("GET", "/v1/machines") => (
            200,
            json!({"machines": [machine("box_1", "ibx-fix-a", "running")], "nextCursor": null}),
        ),
        ("POST", "/v1/machines/box_1/exec") => (
            200,
            json!({"exitCode": 0, "stdout": "fix-a\n", "stderr": "", "timedOut": false, "truncated": false}),
        ),
        _ => (404, json!({})),
    })
    .await;
    let dir = tmp("exec");
    let ib = adapter(&m, &dir);
    let out = ib
        .exec(
            "ibx-fix-a",
            &argv(&["tmux", "ls"]),
            Some(Path::new("/data/home/wt/fix-a")),
        )
        .await
        .unwrap();
    assert_eq!(out.code, 0);
    assert_eq!(out.stdout, "fix-a\n");
    let req = &m.find("POST", "/v1/machines/box_1/exec")[0];
    assert_eq!(req.body["argv"], json!(["tmux", "ls"]));
    assert_eq!(req.body["cwd"], "/data/home/wt/fix-a");
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn retryable_errors_retry_with_the_same_idempotency_key() {
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    let m = serve(move |r| match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/v1/machines/box_1") => (200, machine("box_1", "a", "running")),
        ("POST", "/v1/machines/box_1/suspend") => {
            if n2.fetch_add(1, Ordering::SeqCst) == 0 {
                (
                    503,
                    json!({"error": {"code": "host_unavailable", "message": "later", "retryable": true, "retryAfterSeconds": 0}}),
                )
            } else {
                (
                    202,
                    json!({"id": "op_2", "machineId": "box_1", "type": "suspend", "state": "succeeded"}),
                )
            }
        }
        _ => (404, json!({})),
    })
    .await;
    let dir = tmp("retry");
    adapter(&m, &dir).pause("box_1").await.unwrap();
    let posts = m.find("POST", "/v1/machines/box_1/suspend");
    assert_eq!(posts.len(), 2);
    assert_eq!(
        posts[0].headers["idempotency-key"],
        posts[1].headers["idempotency-key"]
    );
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn non_retryable_error_carries_its_code() {
    let m = serve(|_| {
        (
            403,
            json!({"error": {"code": "missing_scope", "message": "key lacks machines:write", "retryable": false}}),
        )
    })
    .await;
    let dir = tmp("noretry");
    let e = adapter(&m, &dir).list().await.unwrap_err();
    assert_eq!(api::api_code(&e), Some("missing_scope"));
    assert_eq!(m.requests().len(), 1);
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn destroy_confirms_the_id() {
    let m = serve(|r| match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/v1/machines/box_1") => (200, machine("box_1", "a", "running")),
        ("DELETE", "/v1/machines/box_1") => (
            202,
            json!({"id": "op_3", "machineId": "box_1", "type": "delete", "state": "succeeded"}),
        ),
        _ => (404, json!({})),
    })
    .await;
    let dir = tmp("rm");
    adapter(&m, &dir).destroy("box_1").await.unwrap();
    let d = &m.find("DELETE", "/v1/machines/box_1")[0];
    assert_eq!(d.headers["x-confirm-delete"], "box_1");
    assert!(d.headers.contains_key("idempotency-key"));
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn settings_leases_metadata_and_ports() {
    let m = serve(|r| match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/v1/machines/box_1") => (200, machine("box_1", "a", "running")),
        ("PATCH", "/v1/machines/box_1") => (200, machine("box_1", "a", "running")),
        ("PUT", "/v1/machines/box_1/activity-leases/fleet-fix-a") => (
            200,
            json!({"id": "fleet-fix-a", "machineId": "box_1", "expiresAt": "x"}),
        ),
        ("POST", "/v1/machines/box_1/ports") => {
            if r.body["rotateToken"] == json!(true) {
                (200, json!({"url": "https://4000-a.instabox.app", "tokenUrl": "https://4000-a.instabox.app/?_token=ibp_x"}))
            } else {
                (200, json!({"url": "https://4000-a.instabox.app"}))
            }
        }
        _ => (404, json!({})),
    })
    .await;
    let dir = tmp("settings");
    let ib = adapter(&m, &dir);

    ib.set_idle("box_1", Some(Duration::from_secs(600)))
        .await
        .unwrap();
    ib.set_idle("box_1", None).await.unwrap();
    ib.set_max_run("box_1", None).await.unwrap();
    let patches = m.find("PATCH", "/v1/machines/box_1");
    assert_eq!(patches[0].body, json!({"autoSuspendAfterSeconds": 600}));
    assert_eq!(patches[1].body, json!({"autoSuspendAfterSeconds": 0}));
    assert_eq!(patches[2].body, json!({"maxRunMinutes": -1}));

    ib.keepalive("box_1", "fleet-fix-a", Duration::from_secs(7200))
        .await
        .unwrap();
    let lease = &m.find("PUT", "/v1/machines/box_1/activity-leases/fleet-fix-a")[0];
    assert_eq!(
        lease.body["ttlSeconds"], 3600,
        "clamped to InstaBox's 1h max"
    );

    let kv: BTreeMap<String, String> = [
        ("fleet.fix-a.state".to_string(), "wait_plan".to_string()),
        ("fleet.profile".to_string(), String::new()),
    ]
    .into();
    ib.publish("box_1", &kv).await.unwrap();
    let meta = &m.find("PATCH", "/v1/machines/box_1")[3].body["metadata"];
    assert_eq!(meta, &json!({"fleet.fix-a.state": "wait_plan"}));

    let url = ib.port_url("box_1", 4000).await.unwrap();
    assert_eq!(url, "https://4000-a.instabox.app/?_token=ibp_x");
    let ports = m.find("POST", "/v1/machines/box_1/ports");
    assert_eq!(ports[0].body["access"], "private");
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn tty_wakes_an_idle_machine_then_uses_a_pinned_certificate() {
    let woke = Arc::new(AtomicUsize::new(0));
    let w2 = woke.clone();
    let m = serve(move |r| match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/v1/machines/box_1") => {
            if w2.load(Ordering::SeqCst) == 0 {
                let mut v = machine("box_1", "a", "suspended");
                v["autoSuspendedAt"] = json!("2026-11-09T06:00:00Z");
                (200, v)
            } else {
                (200, machine("box_1", "a", "running"))
            }
        }
        ("POST", "/v1/machines/box_1/wake") => {
            w2.fetch_add(1, Ordering::SeqCst);
            (200, machine("box_1", "a", "running"))
        }
        ("POST", "/v1/machines/box_1/connections") => (
            201,
            json!({"id": "cn_1", "machineId": "box_1", "transport": "ssh", "host": "ssh.instabox.sh",
                   "port": 2222, "username": "box_1", "certificate": "ssh-ed25519-cert-v01@openssh.com AAAA",
                   "expiresAt": (Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
                   "hostTrust": {"type": "cert-authority", "publicKey": "ssh-ed25519 CAKEY", "principals": []}}),
        ),
        _ => (404, json!({})),
    })
    .await;
    let dir = tmp("tty");
    let ib = adapter(&m, &dir);
    let cmd = ib
        .tty_command("box_1", &argv(&["tmux", "attach", "-t", "fix a"]))
        .await
        .unwrap();
    assert_eq!(
        woke.load(Ordering::SeqCst),
        1,
        "an idle-paused machine is woken"
    );
    assert_eq!(cmd[0], "ssh");
    assert!(cmd.contains(&"-t".to_string()));
    assert!(cmd.contains(&"StrictHostKeyChecking=yes".to_string()));
    assert!(cmd.contains(&"box_1@ssh.instabox.sh".to_string()));
    assert_eq!(cmd.last().unwrap(), "tmux attach -t 'fix a'");
    let kh = std::fs::read_to_string(dir.join("box_1.known_hosts")).unwrap();
    assert_eq!(
        kh,
        "@cert-authority [ssh.instabox.sh]:2222 ssh-ed25519 CAKEY\n"
    );
    assert!(dir.join("id_ed25519.pub").exists());

    // Cached: a second attach asks for no new certificate.
    ib.tty_command("box_1", &argv(&["true"])).await.unwrap();
    assert_eq!(m.find("POST", "/v1/machines/box_1/connections").len(), 1);
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn cooldown_blocks_attach() {
    let m = serve(|r| match r.path.as_str() {
        "/v1/machines/box_1" => {
            let mut v = machine("box_1", "a", "suspended");
            v["cooldownUntil"] = json!((Utc::now() + chrono::Duration::minutes(9)).to_rfc3339());
            (200, v)
        }
        _ => (404, json!({})),
    })
    .await;
    let dir = tmp("cool");
    let e = adapter(&m, &dir)
        .tty_command("box_1", &argv(&["true"]))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("cooling down"), "{e}");
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn missing_key_names_the_env_var() {
    let api = Api::new("http://127.0.0.1:9", "FLEET_TEST_NO_SUCH_ENV");
    let e = api.get("/v1/machines").await.unwrap_err();
    assert!(e.to_string().contains("$FLEET_TEST_NO_SUCH_ENV"), "{e}");
}
