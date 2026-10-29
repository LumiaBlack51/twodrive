use super::{
    auth::{self, Session},
    http::Api,
};
use crate::{
    model::CloudBackend,
    provider::Provider,
    state::{State, now},
};
use serde_json::{Value, json};
use sha2::Digest;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use url::Url;
fn json_reply(v: Value) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    tiny_http::Response::from_data(serde_json::to_vec(&v).unwrap())
}
use super::*;
use std::{collections::BTreeMap, sync::atomic::AtomicUsize};
use tiny_http::{Response, Server};
#[derive(Clone)]
struct Item {
    id: u64,
    data: Vec<u8>,
    dir: bool,
}
struct Cloud {
    files: BTreeMap<String, Item>,
    slices: BTreeMap<usize, Vec<u8>>,
}
struct Writes {
    root: tempfile::TempDir,
    backend: BaiduDrive,
    cloud: Arc<Mutex<Cloud>>,
    posts: Arc<AtomicUsize>,
    fail: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
fn parent(path: &str) -> &str {
    let p = path.rsplit_once('/').unwrap().0;
    if p.is_empty() { "/" } else { p }
}
fn item_row(path: &str, item: &Item) -> Value {
    json!({"fs_id":item.id,"path":path,"server_filename":path.rsplit('/').next().unwrap(),"size":item.data.len(),"md5":format!("{:x}",md5::Md5::digest(&item.data)),"isdir":u8::from(item.dir),"server_mtime":42})
}
impl Writes {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let state = State::new(root.path().join("account")).unwrap();
        auth::save(
            &state,
            &Session {
                app_key: "synthetic-key".into(),
                secret_key: "synthetic-secret".into(),
                access_token: "synthetic-access".into(),
                refresh_token: "synthetic-refresh".into(),
                expires_at: now() + 3600,
                uk: 7,
                name: "Fixture".into(),
                generation: "test-login".into(),
                identity_pending: false,
            },
        )
        .unwrap();
        let cloud = Arc::new(Mutex::new(Cloud {
            files: BTreeMap::new(),
            slices: BTreeMap::new(),
        }));
        let files = cloud.clone();
        let posts = Arc::new(AtomicUsize::new(0));
        let mutations = posts.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let fail = Arc::new(AtomicBool::new(false));
        let failure = fail.clone();
        let server = Server::http("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", server.server_addr());
        let base = origin.clone();
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                let Some(mut request) = server.recv_timeout(Duration::from_millis(20)).unwrap()
                else {
                    continue;
                };
                let url = Url::parse(&format!("{base}{}", request.url())).unwrap();
                let q: BTreeMap<String, String> = url
                    .query_pairs()
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect();
                assert_eq!(
                    q.get("access_token").map(String::as_str),
                    Some("synthetic-access")
                );
                let mut body = Vec::new();
                request.as_reader().read_to_end(&mut body).unwrap();
                let form: BTreeMap<String, String> = url::form_urlencoded::parse(&body)
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect();
                let mut cloud = files.lock().unwrap();
                let reply = if request.method().as_str() == "POST" {
                    mutations.fetch_add(1, Ordering::Relaxed);
                    if url.path().contains("superfile2") {
                        let start = body.windows(4).position(|b| b == b"\r\n\r\n").unwrap() + 4;
                        let end = body.windows(4).rposition(|b| b == b"\r\n--").unwrap();
                        let data = body[start..end].to_vec();
                        let index = q["partseq"].parse().unwrap();
                        let md5 = format!("{:x}", md5::Md5::digest(&data));
                        cloud.slices.insert(index, data);
                        json_reply(json!({"errno":0,"md5":md5}))
                    } else {
                        match q["method"].as_str() {
                            "precreate" => {
                                cloud.slices.clear();
                                let blocks: Vec<String> =
                                    serde_json::from_str(&form["block_list"]).unwrap();
                                json_reply(
                                    json!({"errno":0,"uploadid":"synthetic-upload","block_list":(0..blocks.len()).collect::<Vec<_>>(),"return_type":1}),
                                )
                            }
                            "create" => {
                                if failure.load(Ordering::Relaxed) {
                                    json_reply(json!({"errno":31023}))
                                } else {
                                    let path = &form["path"];
                                    let dir = form["isdir"] == "1";
                                    let data = if dir {
                                        vec![]
                                    } else {
                                        cloud.slices.values().flatten().copied().collect()
                                    };
                                    let id = cloud
                                        .files
                                        .get(path)
                                        .map_or(cloud.files.len() as u64 + 10, |i| i.id);
                                    let item = Item { id, data, dir };
                                    let row = item_row(path, &item);
                                    cloud.files.insert(path.clone(), item);
                                    let mut row = row;
                                    row["errno"] = json!(0);
                                    json_reply(row)
                                }
                            }
                            "filemanager" => {
                                let rows: Vec<Value> =
                                    serde_json::from_str(&form["filelist"]).unwrap();
                                match q["opera"].as_str() {
                                    "move" => {
                                        let item = rows[0].as_object().unwrap();
                                        let path = item["path"].as_str().unwrap();
                                        let target = format!(
                                            "{}/{}",
                                            item["dest"].as_str().unwrap().trim_end_matches('/'),
                                            item["newname"].as_str().unwrap()
                                        );
                                        let source = cloud.files.remove(path).unwrap();
                                        assert!(!cloud.files.contains_key(&target));
                                        cloud.files.insert(target, source);
                                    }
                                    "delete" => {
                                        cloud.files.remove(rows[0].as_str().unwrap());
                                    }
                                    _ => panic!("unexpected operation"),
                                };
                                json_reply(json!({"errno":0,"info":[{"errno":0}]}))
                            }
                            _ => panic!("unexpected write"),
                        }
                    }
                } else {
                    match url.path() {
                        "/rest/2.0/xpan/nas" => json_reply(json!({"errno":0,"uk":7})),
                        "/rest/2.0/xpan/file" => json_reply(
                            json!({"errno":0,"list":cloud.files.iter().filter(|(p,_)|parent(p)==q["dir"]).map(|(p,i)|item_row(p,i)).collect::<Vec<_>>()}),
                        ),
                        "/rest/2.0/xpan/multimedia" => {
                            let ids: Vec<u64> = serde_json::from_str(&q["fsids"]).unwrap();
                            json_reply(
                                json!({"errno":0,"list":cloud.files.iter().filter(|(_,i)|ids.contains(&i.id)).map(|(p,i)|{let mut row=item_row(p,i);row["dlink"]=json!(format!("{base}/content/{}",i.id));row}).collect::<Vec<_>>()}),
                            )
                        }
                        p if p.starts_with("/content/") => {
                            let id = p.rsplit('/').next().unwrap().parse::<u64>().unwrap();
                            Response::from_data(
                                cloud
                                    .files
                                    .values()
                                    .find(|i| i.id == id)
                                    .unwrap()
                                    .data
                                    .clone(),
                            )
                        }
                        _ => panic!("unexpected read"),
                    }
                };
                drop(cloud);
                request.respond(reply).unwrap();
            }
        });
        let backend = BaiduDrive::connect_with(state, "/", Api::local(origin), &mut || Ok(()))
            .unwrap()
            .with_writable(true);
        Self {
            root,
            backend,
            cloud,
            posts,
            fail,
            stop,
            thread: Some(thread),
        }
    }
    fn put(
        &self,
        path: &str,
        bytes: &[u8],
        old: Option<&crate::model::MetadataEntry>,
    ) -> anyhow::Result<crate::model::MetadataEntry> {
        let input = self.root.path().join("input");
        std::fs::write(&input, bytes).unwrap();
        self.backend.manual_upload(
            path,
            &input,
            old.map(|m| m.remote_id.as_str()),
            old.map(|m| m.etag.as_str()),
            Arc::new(|_, _| Ok(())),
        )
    }
}
impl Drop for Writes {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}
#[test]
fn manual_baidu_writes_slice_verify_preserve_original_and_manage_opaque_ids() {
    let f = Writes::new();
    let data = vec![b'x'; 4 * 1024 * 1024 + 13];
    let first = f.put("/file.bin", &data, None).unwrap();
    let second = f.put("/file.bin", b"edited", Some(&first)).unwrap();
    assert_eq!(f.backend.download(&second.remote_id).unwrap(), b"edited");
    let recovery = f.root.path().join("account/recovery");
    assert!(
        std::fs::read_dir(recovery)
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| std::fs::read(e.path().join("before.bin")).is_ok_and(|b| b == data))
    );
    f.backend.create_folder("/folder").unwrap();
    let moved = f
        .backend
        .rename(&second.remote_id, "/folder/new.bin")
        .unwrap();
    f.backend.delete(&moved.remote_id).unwrap();
    f.backend
        .delete(
            &f.backend
                .get_metadata_by_path("/folder")
                .unwrap()
                .unwrap()
                .remote_id,
        )
        .unwrap();
    assert!(f.cloud.lock().unwrap().files.is_empty());
    assert!(!f.backend.capabilities().conditional_upload);
    assert!(!f.backend.capabilities().backup());
}
#[test]
fn stale_baidu_snapshot_refuses_mutation_and_failure_retains_edit_and_original() {
    let f = Writes::new();
    let first = f.put("/file", b"before", None).unwrap();
    f.cloud.lock().unwrap().files.get_mut("/file").unwrap().data = b"external".to_vec();
    let posts = f.posts.load(Ordering::Relaxed);
    assert!(f.put("/file", b"stale edit", Some(&first)).is_err());
    assert_eq!(f.posts.load(Ordering::Relaxed), posts);
    let current = f.backend.get_metadata_by_path("/file").unwrap().unwrap();
    f.fail.store(true, Ordering::Relaxed);
    assert!(
        f.put("/file", b"recover this edit", Some(&current))
            .is_err()
    );
    assert_eq!(f.cloud.lock().unwrap().files["/file"].data, b"external");
    let recovery = f.root.path().join("account/recovery");
    assert!(
        std::fs::read_dir(recovery)
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| std::fs::read(e.path().join("edited.bin"))
                .is_ok_and(|b| b == b"recover this edit")
                && std::fs::read(e.path().join("before.bin")).is_ok_and(|b| b == b"external"))
    );
}
