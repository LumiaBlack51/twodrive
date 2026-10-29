//! Synthetic component only: no Apple account or file contents are used.
use super::*;
use crate::{provider::Backend, readonly_mount::Bridge};
use std::os::unix::fs::PermissionsExt;

fn fixture() -> (tempfile::TempDir, RcloneDrive, PathBuf) {
    let (root, mut backend) = super::tests::fixture();
    backend.writable = true;
    backend.binding = format!("icloud:{:x}:epoch-a", Sha256::digest(b"synthetic-account"));
    backend
        .state
        .save(
            MANIFEST,
            &Auth {
                kind: "icloud".into(),
                binding: backend.binding.clone(),
                ready: true,
            },
        )
        .unwrap();
    twodrive_core::private_file::write_secret(
        &backend.state.dir.join(CONFIG),
        &[HEADER, b"\nSYNTHETIC ENCRYPTED FIXTURE ONLY"].concat(),
    )
    .unwrap();
    let data = root.path().join("synthetic-objects.json");
    fs::write(&data, serde_json::to_vec(&serde_json::json!({"ops":[], "files":{"/before.txt":{"ID":"synthetic-object","IsDir":false,"Size":7,"ModTime":"2026-10-28T00:00:00Z"}}})).unwrap()).unwrap();
    let script = r#"#!/usr/bin/python3
import json,pathlib,sys
p=pathlib.Path(@STATE@)
s=json.loads(p.read_text())
args=sys.argv[1:]
if 'config' in args and 'dump' in args:
    print(json.dumps({'twodrive':{'type':'iclouddrive','apple_id':s.get('refresh_id','synthetic-account')}}));sys.exit(0)
if 'rcd' in args:
    import http.server,os,base64
    s['launches']=s.get('launches',0)+1
    p.write_text(json.dumps(s))
    address=args[args.index('--rc-addr')+1]
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self,*args): pass
        def do_POST(self):
            auth='Basic '+base64.b64encode((os.environ['RCLONE_RC_USER']+':'+os.environ['RCLONE_RC_PASS']).encode()).decode()
            if self.headers.get('Authorization')!=auth:
                self.send_response(401);self.end_headers();return
            body=json.loads(self.rfile.read(int(self.headers.get('Content-Length',0))))
            value=json.loads(p.read_text())
            status=200
            if self.path=='/core/pid': result={'pid':os.getpid()}
            else:
                value['ops'].append(self.path.lstrip('/'))
                def row(path,v):return dict(v,Name=path.rsplit('/',1)[1],Path=path.rsplit('/',1)[1])
                if self.path=='/operations/list':
                    parent=body['remote']
                    result={'list':[row(k,v) for k,v in value['files'].items() if k.rsplit('/',1)[0].lstrip('/')==parent]}
                elif self.path=='/operations/movefile':
                    source='/'+body['srcRemote'];dest='/'+body['dstRemote']
                    assert body['srcFs']==body['dstFs']=='twodrive:' and '--ignore-existing' in args
                    if value.get('lost_reply'):
                        value['files'][dest]=value['files'].pop(source)
                        p.write_text(json.dumps(value));self.connection.close();return
                    if value.get('late_collision'):
                        value['files'][dest]=dict(value['files'][source],ID='other-object')
                    elif source not in value['files'] or dest in value['files']:status=409
                    else:value['files'][dest]=value['files'].pop(source)
                    result={}
                else:status=400;result={}
                if value.get('refresh'):
                    config=pathlib.Path(args[args.index('--config')+1]);config.write_bytes(config.read_bytes()+b'\nSYNTHETIC REFRESH')
                if value.get('newer_canonical'):
                    canonical=pathlib.Path(value['canonical_path']);canonical.write_bytes(b'# Encrypted rclone configuration File\nSYNTHETIC NEWER CONFIG')
                p.write_text(json.dumps(value))
            data=json.dumps(result).encode();self.send_response(status);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
    host,port=address.rsplit(':',1)
    http.server.HTTPServer((host,int(port)),Handler).serve_forever()
s['launches']=s.get('launches',0)+1
op=next((v for v in args if v in ['lsjson','moveto']),None)
assert op is not None
s['ops'].append(op)
p.write_text(json.dumps(s))
i=args.index(op)
source=args[i+1].removeprefix('twodrive:')
source='/' + source.lstrip('/')
if op=='lsjson':
    def row(path,v):
        return dict(v,Name=path.rsplit('/',1)[1],Path=path.rsplit('/',1)[1])
    if '--stat' in args:
        if source not in s['files']: sys.exit(3)
        print(json.dumps(row(source,s['files'][source])))
    else:
        print(json.dumps([row(k,v) for k,v in s['files'].items() if k.rsplit('/',1)[0]==source.rstrip('/')]))
else:
    dest='/' + args[i+2].removeprefix('twodrive:').lstrip('/')
    assert '--ignore-existing' in args
    assert source in s['files'] and dest not in s['files']
    s['files'][dest]=s['files'].pop(source)
    p.write_text(json.dumps(s))
"#.replace("@STATE@", &serde_json::to_string(&data).unwrap());
    let exe = root.path().join("synthetic-component");
    fs::write(&exe, script).unwrap();
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
    backend.exe = exe;
    backend.initial_root=vec![backend.entry("/before.txt", &serde_json::json!({"ID":"synthetic-object","IsDir":false,"Size":7,"ModTime":"2026-10-28T00:00:00Z"})).unwrap()];
    (root, backend, data)
}
#[test]
fn mounted_same_directory_rename_uses_one_fresh_preflight_and_one_confirmation() {
    let (root, backend, data) = fixture();
    let bridge = Bridge::start_writable(
        Backend::new(backend),
        root.path(),
        &root.path().join("recovery"),
    )
    .unwrap();
    let c = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();
    let reply = c
        .request(
            reqwest::Method::from_bytes(b"MOVE").unwrap(),
            format!("http://{}/before.txt", bridge.address),
        )
        .bearer_auth(&bridge.token)
        .header(
            "Destination",
            format!("http://{}/after.txt", bridge.address),
        )
        .send()
        .unwrap();
    assert_eq!(reply.status(), 204);
    let result: Value = serde_json::from_slice(&fs::read(data).unwrap()).unwrap();
    assert!(
        result["files"]["/after.txt"].is_object() && result["files"].get("/before.txt").is_none()
    );
    assert_eq!(
        result["launches"], 1,
        "reuse one authenticated component for the transaction"
    );
    assert_eq!(
        result["ops"],
        serde_json::json!(["operations/list", "operations/movefile", "operations/list"])
    );
    let timing: Value = serde_json::from_slice(
        &fs::read(root.path().join("private/last-rename-timing.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(timing["remote_confirmed"], true);
    assert!(timing["move_ms"].is_u64());
    assert!(
        !timing.to_string().contains("before.txt")
            && !timing.to_string().contains("synthetic-object")
    );
}

#[test]
fn coalesced_rename_refuses_external_changes_collisions_and_denied_access() {
    for scenario in [
        "version",
        "collision",
        "binding",
        "readonly",
        "paused",
        "type",
    ] {
        let (root, backend, data) = fixture();
        let expected = backend.initial_root[0].clone();
        let mut contents: Value = serde_json::from_slice(&fs::read(&data).unwrap()).unwrap();
        match scenario {
            "version" => {
                contents["files"]["/before.txt"]["ModTime"] =
                    serde_json::json!("2030-01-01T00:00:00Z")
            }
            "collision" => {
                contents["files"]["/after.txt"] = serde_json::json!({"ID":"other-object","IsDir":false,"Size":9,"ModTime":"2026-10-28T00:00:00Z"})
            }
            "binding" => backend
                .state
                .save(
                    MANIFEST,
                    &Auth {
                        kind: "icloud".into(),
                        binding: "icloud:other:epoch-b".into(),
                        ready: true,
                    },
                )
                .unwrap(),
            _ => {}
        }
        fs::write(&data, serde_json::to_vec(&contents).unwrap()).unwrap();
        let mut provider = Backend::new(backend);
        if scenario == "readonly" {
            provider = provider.with_write_access(false, None);
        }
        if scenario == "paused" {
            let pause = root.path().join("paused");
            fs::write(&pause, b"paused").unwrap();
            provider = provider.with_pause_file(pause);
        }
        assert!(
            provider
                .rename_snapshot(
                    "/before.txt",
                    "/after.txt",
                    &expected.remote_id,
                    &expected.etag,
                    scenario == "type"
                )
                .is_err(),
            "{scenario}"
        );
        let now: Value = serde_json::from_slice(&fs::read(&data).unwrap()).unwrap();
        assert!(
            now["ops"]
                .as_array()
                .unwrap()
                .iter()
                .all(|op| op != "moveto" && op != "operations/movefile"),
            "{scenario}"
        );
        assert_eq!(
            now["files"], contents["files"],
            "{scenario} must preserve every source/target"
        );
    }
}

#[test]
fn snapshot_rename_preserves_cross_directory_checks_and_parent_refresh() {
    let (root, backend, data) = fixture();
    let expected = backend.initial_root[0].clone();
    let mut contents: Value = serde_json::from_slice(&fs::read(&data).unwrap()).unwrap();
    contents["files"]["/folder"] = serde_json::json!({"ID":"folder-object","IsDir":true,"Size":0,"ModTime":"2026-10-28T00:00:00Z"});
    fs::write(&data, serde_json::to_vec(&contents).unwrap()).unwrap();
    let provider = Backend::new(backend);
    let moved = provider
        .rename_snapshot(
            "/before.txt",
            "/folder/after.txt",
            &expected.remote_id,
            &expected.etag,
            false,
        )
        .unwrap();
    assert_eq!(moved.path, "/folder/after.txt");
    let now: Value = serde_json::from_slice(&fs::read(data).unwrap()).unwrap();
    assert!(now["files"].get("/before.txt").is_none());
    assert_eq!(
        now["ops"],
        serde_json::json!(["lsjson", "lsjson", "moveto", "lsjson"])
    );
    drop(root);
}

#[test]
fn session_rename_checks_post_state_and_never_retries_an_uncertain_move() {
    for flag in ["late_collision", "lost_reply"] {
        let (_root, backend, data) = fixture();
        let expected = backend.initial_root[0].clone();
        let mut value: Value = serde_json::from_slice(&fs::read(&data).unwrap()).unwrap();
        value[flag] = serde_json::json!(true);
        fs::write(&data, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(
            Backend::new(backend)
                .rename_snapshot(
                    "/before.txt",
                    "/after.txt",
                    &expected.remote_id,
                    &expected.etag,
                    false
                )
                .is_err()
        );
        let result: Value = serde_json::from_slice(&fs::read(data).unwrap()).unwrap();
        assert_eq!(
            result["ops"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|op| **op == "operations/movefile")
                .count(),
            1,
            "never replay a mutation with a lost reply"
        );
        if flag == "late_collision" {
            assert!(result["files"]["/before.txt"].is_object());
        } else {
            assert!(result["files"]["/after.txt"].is_object());
        }
    }
}

#[test]
fn session_refresh_checks_identity_keeps_newer_config_and_cleans_private_children() {
    for scenario in ["refresh", "wrong_identity", "newer"] {
        let (root, backend, data) = fixture();
        let expected = backend.initial_root[0].clone();
        let config = backend.state.dir.join(CONFIG);
        let before = fs::read(&config).unwrap();
        let mut contents: Value = serde_json::from_slice(&fs::read(&data).unwrap()).unwrap();
        contents["refresh"] = serde_json::json!(true);
        if scenario == "wrong_identity" {
            contents["refresh_id"] = serde_json::json!("other-account");
        }
        if scenario == "newer" {
            contents["newer_canonical"] = serde_json::json!(true);
            contents["canonical_path"] = serde_json::json!(config);
        }
        fs::write(&data, serde_json::to_vec(&contents).unwrap()).unwrap();
        let result = Backend::new(backend).rename_snapshot(
            "/before.txt",
            "/after.txt",
            &expected.remote_id,
            &expected.etag,
            false,
        );
        match scenario {
            "refresh" => {
                assert!(result.is_ok());
                assert!(fs::read(&config).unwrap().ends_with(b"SYNTHETIC REFRESH"));
            }
            "wrong_identity" => {
                assert!(result.is_err());
                assert_eq!(fs::read(&config).unwrap(), before);
            }
            "newer" => {
                assert!(result.is_ok());
                assert!(
                    fs::read(&config)
                        .unwrap()
                        .ends_with(b"SYNTHETIC NEWER CONFIG")
                );
            }
            _ => unreachable!(),
        }
        assert!(fs::read_dir(root.path().join("private")).unwrap().all(|e| {
            !e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("rename-session-")
        }));
    }
}
