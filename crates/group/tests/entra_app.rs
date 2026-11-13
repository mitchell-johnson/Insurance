//! The script that makes the group service's Entra ID app registration
//! (`deploy/entra/group-app.sh`), run against a stand-in for the Azure CLI
//! that keeps what it was asked to make.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A stand-in `az`: app registrations, service principals and role
/// assignments kept as files, every call logged.
const FAKE_AZ: &str = r#"#!/usr/bin/env bash
set -euo pipefail
state=${FAKE_AZ_STATE:?}
echo "$*" >>"$state/log"
arg() { local want=$1; shift; while [ "$#" -gt 0 ]; do if [ "$1" = "$want" ]; then echo "$2"; return; fi; shift; done; }
key() { printf '%s' "$1" | tr -c 'A-Za-z0-9' '_'; }
case "$1 $2 ${3:-}" in
  "ad app list") f="$state/app_$(key "$(arg --identifier-uri "$@")")"; [ -f "$f" ] && cat "$f" || true ;;
  "ad app create")
    uri=$(arg --identifier-uris "$@"); id="app-$(key "$uri")"
    echo "$id" >"$state/app_$(key "$uri")"; cp "$(arg --app-roles "$@" | sed 's/^@//')" "$state/roles_$id"; echo "$id" ;;
  "ad app update") id=$(arg --id "$@"); cp "$(arg --app-roles "$@" | sed 's/^@//')" "$state/roles_$id" ;;
  "ad sp list") app=$(arg --filter "$@" | sed "s/appId eq '\(.*\)'/\1/"); f="$state/sp_$app"; [ -f "$f" ] && cat "$f" || true ;;
  "ad sp create") app=$(arg --id "$@"); echo "sp-$app" >"$state/sp_$app"; echo "sp-$app" ;;
  "ad sp show") cat "$state/roles_of_$(arg --id "$@")" 2>/dev/null || true ;;
  rest*)
    url=$(arg --url "$@"); principal=$(echo "$url" | sed 's|.*/servicePrincipals/\([^/]*\)/appRoleAssignments|\1|')
    touch "$state/assignments"
    if [ "$(arg --method "$@")" = "GET" ]; then
      q=$(arg --query "$@"); id=$(echo "$q" | sed "s/.*appRoleId=='\([^']*\)'.*/\1/"); res=$(echo "$q" | sed "s/.*resourceId=='\([^']*\)'.*/\1/")
      grep -c "^$principal $id $res\$" "$state/assignments" || true
    else
      body=$(arg --body "$@"); echo "$(jq -r '.principalId + " " + .appRoleId + " " + .resourceId' <<<"$body")" >>"$state/assignments"
    fi ;;
  *) echo "unexpected: $*" >&2; exit 9 ;;
esac
"#;

struct Rig {
    dir: PathBuf,
}

impl Rig {
    fn new(test: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("ic-group-entra-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::create_dir_all(dir.join("state")).unwrap();
        let az = dir.join("bin/az");
        std::fs::write(&az, FAKE_AZ).unwrap();
        Command::new("chmod").arg("+x").arg(&az).status().unwrap();
        Rig { dir }
    }

    fn state(&self, file: &str) -> PathBuf {
        self.dir.join("state").join(file)
    }

    fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.state(file)).unwrap_or_default()
    }

    fn run(&self, args: &[&str]) -> Output {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/entra/group-app.sh");
        let path = format!("{}:{}", self.dir.join("bin").display(), std::env::var("PATH").unwrap_or_default());
        Command::new("bash")
            .arg(script)
            .args(args)
            .env("PATH", path)
            .env("FAKE_AZ_STATE", self.dir.join("state"))
            .output()
            .unwrap()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn jq_available() -> bool {
    Command::new("jq").arg("--version").output().is_ok_and(|o| o.status.success())
}

/// Purpose: the script makes the group service's app registration with one application-only
/// app role per caller name, under fixed role IDs; gives each named calling service's
/// identity the role asked for; gives the group service's own identity the customer
/// master's `group-admin` role; and prints the audience for the service's settings. Run
/// again, it makes nothing new and grants nothing twice.
/// Why it matters: the service refuses every caller whose token lacks the role it acts
/// under, so a registration made by hand with one role mistyped shuts a portal out, and a
/// role given to the wrong identity lets a service act as another. A rerun that made a
/// second registration would leave callers with tokens for one and the service checking
/// the other.
#[test]
fn the_app_registration_is_made_once_with_its_roles_and_grants() {
    if !jq_available() {
        panic!("jq is needed by deploy/entra/group-app.sh and by this test");
    }
    let rig = Rig::new("once");
    std::fs::write(rig.state("app_api___ic_customers"), "cm-app\n").unwrap();
    std::fs::write(rig.state("sp_cm-app"), "cm-sp\n").unwrap();
    std::fs::write(rig.state("roles_of_cm-sp"), "cm-group-admin\n").unwrap();
    let args = [
        "ic",
        "group-mi",
        "employer-portal=portal-mi",
        "member-portal=portal-mi",
        "underwriting=uw-mi",
        "customers=api://ic-customers",
    ];
    let first = rig.run(&args);
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    assert_eq!(String::from_utf8_lossy(&first.stdout).trim(), "api://ic-group");

    let roles: serde_json::Value = serde_json::from_str(&rig.read("roles_app-api___ic_group")).unwrap();
    let names: Vec<&str> = roles.as_array().unwrap().iter().map(|r| r["value"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "group-admin",
            "employer-portal",
            "member-portal",
            "underwriting",
            "reinsurance",
            "cash-matching"
        ]
    );
    assert!(roles
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["allowedMemberTypes"] == serde_json::json!(["Application"])));
    let id_of = |name: &str| {
        roles.as_array().unwrap().iter().find(|r| r["value"] == name).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let service = "sp-app-api___ic_group";
    let mut granted: Vec<String> = rig.read("assignments").lines().map(str::to_string).collect();
    granted.sort();
    let mut expected = vec![
        format!("portal-mi {} {service}", id_of("employer-portal")),
        format!("portal-mi {} {service}", id_of("member-portal")),
        format!("uw-mi {} {service}", id_of("underwriting")),
        "group-mi cm-group-admin cm-sp".to_string(),
    ];
    expected.sort();
    assert_eq!(granted, expected);

    // Again: the registration is kept, its roles brought up to date, nothing granted twice.
    let creates = |rig: &Rig| {
        rig.read("log")
            .lines()
            .filter(|l| l.starts_with("ad app create") || l.starts_with("ad sp create"))
            .count()
    };
    assert_eq!(creates(&rig), 2);
    let second = rig.run(&args);
    assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
    assert_eq!(creates(&rig), 2);
    assert!(rig.read("log").lines().any(|l| l.starts_with("ad app update")));
    assert_eq!(rig.read("assignments").lines().count(), 4);
}

/// Purpose: the script checks every argument before it changes anything: a caller name the
/// service does not take, an argument that is not role=principal, and too few arguments are
/// each refused with nothing made; and a customer master with no registration, or without
/// a `group-admin` role, stops it with an error rather than a silent skip.
/// Why it matters: half a registration (the app made, a role missing) is worse than none,
/// because it looks done. And the customer master grant is what lets the service match
/// joiners; skipped silently, every file would stop at matching in production.
#[test]
fn the_app_registration_script_refuses_what_it_cannot_do_whole() {
    if !jq_available() {
        panic!("jq is needed by deploy/entra/group-app.sh and by this test");
    }
    let rig = Rig::new("refuses");
    for args in [
        vec!["ic"],
        vec!["ic", "group-mi", "payroll=x-mi"],
        vec!["ic", "group-mi", "employer-portal"],
        vec!["ic", "group-mi", "employer-portal="],
    ] {
        let out = rig.run(&args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
    assert_eq!(rig.read("log"), "", "nothing was asked of Azure");

    let missing = rig.run(&["ic", "group-mi", "customers=api://ic-customers"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no app registration for the customer master"));
    std::fs::write(rig.state("app_api___ic_customers"), "cm-app\n").unwrap();
    std::fs::write(rig.state("sp_cm-app"), "cm-sp\n").unwrap();
    let no_role = rig.run(&["ic", "group-mi", "customers=api://ic-customers"]);
    assert_eq!(no_role.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&no_role.stderr).contains("has no group-admin role"));
}
