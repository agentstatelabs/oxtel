//! Resource detection, in this process and, where the environment matters,
//! in child processes with the environment set.

use std::process::Command;

use oxtel_sdk::resource::Detector;
use oxtel_sdk::{KeyValue, Resource, Value};

const CHILD: &str = "OXTEL_TEST_CHILD";

/// Run test `name` in a child process with `vars` set, and the variables
/// that would change the outcome cleared.
fn child(name: &str, vars: &[(&str, &str)]) {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", name, "--test-threads=1"])
        .env(CHILD, "1");
    for var in [
        "OTEL_EXPERIMENTAL_RESOURCE_DETECTORS",
        "OTEL_RESOURCE_ATTRIBUTES",
        "OTEL_SERVICE_NAME",
        "KUBERNETES_SERVICE_HOST",
        "K8S_NODE_NAME",
    ] {
        command.env_remove(var);
    }
    for (k, v) in vars {
        command.env(k, v);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "child {name} failed:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

fn in_child() -> bool {
    std::env::var_os(CHILD).is_some()
}

fn string(r: &Resource, key: &str) -> Option<String> {
    match r.get(key)? {
        Value::String(s) => Some(s.clone()),
        other => Some(format!("{other:?}")),
    }
}

#[test]
fn the_default_resource_describes_the_process_and_machine() {
    if !in_child() {
        return child(
            "the_default_resource_describes_the_process_and_machine",
            &[],
        );
    }
    let r = Resource::builder().build();
    assert_eq!(
        r.get("process.pid"),
        Some(&Value::I64(i64::from(std::process::id())))
    );
    let instance = string(&r, "service.instance.id").expect("an instance id");
    assert_eq!(instance.len(), 36);
    assert!(r.get("host.arch").is_some());
    let os = string(&r, "os.type").unwrap();
    assert!(
        ["linux", "darwin", "windows", "freebsd"].contains(&os.as_str()),
        "{os}"
    );
    let exe = string(&r, "process.executable.name").unwrap();
    assert!(exe.starts_with("resource_detection"), "{exe}");
    if cfg!(target_os = "linux") {
        assert!(r.get("host.name").is_some());
    }
    assert!(r.get("k8s.pod.name").is_none(), "not in Kubernetes");
    assert_ne!(
        Resource::builder()
            .build()
            .get("service.instance.id")
            .cloned(),
        None
    );
}

#[test]
fn detectors_are_chosen_by_the_environment() {
    if !in_child() {
        child(
            "detectors_are_chosen_by_the_environment",
            &[("OTEL_EXPERIMENTAL_RESOURCE_DETECTORS", "process, OS")],
        );
        return;
    }
    let r = Resource::builder().build();
    assert!(r.get("process.pid").is_some() && r.get("os.type").is_some());
    assert!(r.get("host.arch").is_none() && r.get("service.instance.id").is_none());
}

#[test]
fn none_turns_detection_off() {
    if !in_child() {
        child(
            "none_turns_detection_off",
            &[("OTEL_EXPERIMENTAL_RESOURCE_DETECTORS", "none")],
        );
        return;
    }
    let r = Resource::builder().build();
    assert!(r.get("process.pid").is_none() && r.get("host.arch").is_none());
    assert!(
        r.get("service.name").is_some(),
        "the SDK's own attributes stay"
    );
}

#[test]
fn the_environment_overrides_what_was_detected() {
    if !in_child() {
        child(
            "the_environment_overrides_what_was_detected",
            &[(
                "OTEL_RESOURCE_ATTRIBUTES",
                "host.arch=custom,service.instance.id=pod-7",
            )],
        );
        return;
    }
    let r = Resource::builder().build();
    assert_eq!(string(&r, "host.arch").as_deref(), Some("custom"));
    assert_eq!(string(&r, "service.instance.id").as_deref(), Some("pod-7"));
}

#[test]
fn kubernetes_is_detected_inside_a_pod() {
    if !in_child() {
        child(
            "kubernetes_is_detected_inside_a_pod",
            &[
                ("KUBERNETES_SERVICE_HOST", "10.96.0.1"),
                ("HOSTNAME", "checkout-7d9f8b-x2x4q"),
                ("K8S_NODE_NAME", "node-3"),
                ("OTEL_EXPERIMENTAL_RESOURCE_DETECTORS", "k8s"),
            ],
        );
        return;
    }
    let r = Resource::builder().build();
    assert_eq!(
        string(&r, "k8s.pod.name").as_deref(),
        Some("checkout-7d9f8b-x2x4q")
    );
    assert_eq!(string(&r, "k8s.node.name").as_deref(), Some("node-3"));
}

struct Cloud;

impl Detector for Cloud {
    fn detect(&self) -> Vec<KeyValue> {
        vec![
            KeyValue::new("cloud.provider", "example"),
            KeyValue::new("cloud.region", "eu-1"),
        ]
    }
}

#[test]
fn detectors_of_your_own_run_and_without_env_runs_none_of_the_built_ins() {
    let r = Resource::builder()
        .without_env()
        .with_detector(Cloud)
        .build();
    assert_eq!(string(&r, "cloud.region").as_deref(), Some("eu-1"));
    assert!(
        r.get("process.pid").is_none(),
        "without_env skips built-ins"
    );
    let r = Resource::builder()
        .without_detectors()
        .with_attribute(KeyValue::new("cloud.region", "us-2"))
        .with_detector(Cloud)
        .build();
    assert_eq!(
        string(&r, "cloud.region").as_deref(),
        Some("us-2"),
        "the builder's own attributes win"
    );
    assert!(r.get("host.arch").is_none());
}
