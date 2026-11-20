use super::*;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Read;
use std::time::SystemTime;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use url::Url;

const HISTORY: &str = "gnome-opened.json";
const RECENT_LIMIT: u64 = 4 * 1024 * 1024;
const HISTORY_LIMIT: usize = 100;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct Opened {
    path: String,
    opened_at: i64,
}

#[derive(Deserialize, Serialize)]
struct History {
    revision: String,
    binding: String,
    files: Vec<Opened>,
}

fn load(dir: &Path, revision: &str, binding: &str) -> Option<History> {
    let path = dir.join(HISTORY);
    let info = fs::symlink_metadata(&path).ok()?;
    if !info.is_file() || info.len() > RECENT_LIMIT {
        return None;
    }
    let history: History = serde_json::from_reader(File::open(path).ok()?).ok()?;
    (history.revision == revision && history.binding == binding).then_some(history)
}

fn remote_path(href: &str, mount: &Url, alias: &Path, export: &Path) -> Option<String> {
    let url = Url::parse(href).ok()?;
    if url.query().is_some() || url.fragment().is_some() {
        return None;
    }
    let path = if url.scheme() == "file" {
        let local = url.to_file_path().ok()?;
        let relative = local
            .strip_prefix(alias)
            .or_else(|_| local.strip_prefix(export))
            .ok()?;
        format!("/{}", relative.to_str()?)
    } else if url.scheme() == mount.scheme()
        && url.host_str() == mount.host_str()
        && url.port() == mount.port()
        && url.username() == mount.username()
        && url.password().is_none()
    {
        percent_encoding::percent_decode_str(url.path())
            .decode_utf8()
            .ok()?
            .into_owned()
    } else {
        return None;
    };
    (path != "/" && path.len() <= 4096 && twodrive_dev::provider::valid_path(&path).is_ok())
        .then_some(path)
}

fn opens(
    xml: &str,
    mount: &Url,
    alias: &Path,
    export: &Path,
    since: i64,
) -> anyhow::Result<Vec<Opened>> {
    const NS: &str = "http://www.freedesktop.org/standards/desktop-bookmarks";
    let document = roxmltree::Document::parse(xml)?;
    Ok(document
        .root_element()
        .children()
        .filter(|node| node.has_tag_name("bookmark"))
        .filter_map(|node| {
            let path = remote_path(node.attribute("href")?, mount, alias, export)?;
            if node.descendants().any(|n| {
                n.has_tag_name("mime-type") && n.attribute("type") == Some("inode/directory")
            }) {
                return None;
            }
            let opened_at = node
                .descendants()
                .filter(|n| {
                    n.has_tag_name((NS, "application"))
                        && n.attribute("name") == Some("org.gnome.Nautilus")
                })
                .filter_map(|n| OffsetDateTime::parse(n.attribute("modified")?, &Rfc3339).ok())
                .map(|t| t.unix_timestamp())
                .max()?;
            (opened_at >= since).then_some(Opened { path, opened_at })
        })
        .collect())
}

pub(super) struct Journal {
    history: History,
    recent: Option<PathBuf>,
    modified: Option<SystemTime>,
    mount: Url,
    since: i64,
}

impl Journal {
    pub(super) fn new(
        cache: &State,
        p: &Profile,
        binding: &str,
        uri: &str,
    ) -> anyhow::Result<Self> {
        let recent = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })
            .map(|path| path.join("recently-used.xbel"));
        Ok(Self {
            history: load(&cache.dir, &p.revision, binding).unwrap_or_else(|| History {
                revision: p.revision.clone(),
                binding: binding.into(),
                files: Vec::new(),
            }),
            recent,
            modified: None,
            mount: Url::parse(uri)?,
            since: OffsetDateTime::now_utc().unix_timestamp(),
        })
    }

    pub(super) fn refresh(
        &mut self,
        cache: &State,
        alias: &Path,
        export: &Path,
    ) -> anyhow::Result<()> {
        let Some(recent) = &self.recent else {
            return Ok(());
        };
        let Ok(info) = fs::symlink_metadata(recent) else {
            return Ok(());
        };
        if !info.is_file() || info.len() > RECENT_LIMIT {
            return Ok(());
        }
        let modified = info.modified()?;
        if self.modified == Some(modified) {
            return Ok(());
        }
        let mut xml = String::new();
        File::open(recent)?
            .take(RECENT_LIMIT + 1)
            .read_to_string(&mut xml)?;
        ensure!(
            xml.len() as u64 <= RECENT_LIMIT,
            "recent files list too large"
        );
        let mut files = opens(&xml, &self.mount, alias, export, self.since)?;
        files.extend(self.history.files.iter().cloned());
        files.sort_unstable_by(|a, b| {
            b.opened_at
                .cmp(&a.opened_at)
                .then_with(|| a.path.cmp(&b.path))
        });
        let mut seen = std::collections::HashSet::new();
        files.retain(|item| seen.insert(item.path.clone()));
        files.truncate(HISTORY_LIMIT);
        if files != self.history.files {
            let updated = History {
                files,
                revision: self.history.revision.clone(),
                binding: self.history.binding.clone(),
            };
            cache.save(HISTORY, &updated)?;
            self.history = updated;
        }
        self.modified = Some(modified);
        Ok(())
    }
}

pub(in super::super::super) fn rows(app: &App) -> Vec<Value> {
    let mut rows = Vec::new();
    for profile in app.config.profiles.iter().filter(|p| p.kind == "icloud") {
        let Some(session) = app.saved_session(profile) else {
            continue;
        };
        let Some(binding) = session["binding"].as_str() else {
            continue;
        };
        let dir = app.mount_state(profile).join("linux-readonly");
        let Some(history) = load(&dir, &profile.revision, binding) else {
            continue;
        };
        for item in history.files.into_iter().take(HISTORY_LIMIT) {
            if twodrive_dev::provider::valid_path(&item.path).is_err() || item.path == "/" {
                continue;
            }
            rows.push(json!({
                "profile": profile.id, "revision": profile.revision, "account": profile.name,
                "path": item.path, "name": item.path.rsplit('/').next(), "kind": "download",
                "source": "nautilus", "state": "已打开", "opened_at": item.opened_at,
            }));
        }
    }
    rows.sort_unstable_by_key(|row| std::cmp::Reverse(row["opened_at"].as_i64()));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_current_icloud_files_opened_by_nautilus_survive_remount() {
        let root = tempfile::tempdir().unwrap();
        let cache = State::new(root.path().join("account")).unwrap();
        let recent = root.path().join("recently-used.xbel");
        let uri = "dav://iCloud-abcdefgh@127.0.0.1:31000/";
        let alias = Path::new("/cloud/iCloud");
        let export = Path::new("/run/user/1000/gvfs/dav:fixture");
        fs::write(&recent, r#"<xbel xmlns:b="http://www.freedesktop.org/standards/desktop-bookmarks">
            <bookmark href="dav://iCloud-abcdefgh@127.0.0.1:31000/%E6%96%87%E6%A1%A3%20%23.docx"><b:application name="org.gnome.Nautilus" modified="2026-11-11T01:00:00Z"/></bookmark>
            <bookmark href="dav://iCloud-other000@127.0.0.1:31000/other"><b:application name="org.gnome.Nautilus" modified="2026-11-11T01:00:00Z"/></bookmark>
            <bookmark href="file:///cloud/iCloud/sibling.txt"><b:application name="org.gnome.Nautilus" modified="2026-11-11T01:00:01Z"/></bookmark>
            <bookmark href="file:///cloud/iCloud/thumbnail.txt"><b:application name="thumbnailer" modified="2026-11-11T01:00:00Z"/></bookmark>
            <bookmark href="file:///cloud/iCloud-elsewhere/other"><b:application name="org.gnome.Nautilus" modified="2026-11-11T01:00:00Z"/></bookmark>
            <bookmark href="file:///cloud/iCloud/old.txt"><b:application name="org.gnome.Nautilus" modified="2026-11-10T01:00:00Z"/></bookmark>
        </xbel>"#).unwrap();
        let since = OffsetDateTime::parse("2026-11-11T00:00:00Z", &Rfc3339)
            .unwrap()
            .unix_timestamp();
        let mut journal = Journal {
            history: History {
                revision: "rev".into(),
                binding: "binding".into(),
                files: Vec::new(),
            },
            recent: Some(recent),
            modified: None,
            mount: Url::parse(uri).unwrap(),
            since,
        };
        journal.refresh(&cache, alias, export).unwrap();
        let stored = load(&cache.dir, "rev", "binding").unwrap();
        assert_eq!(
            stored
                .files
                .iter()
                .map(|item| item.path.as_str())
                .collect::<Vec<_>>(),
            ["/sibling.txt", "/文档 #.docx"]
        );
        assert!(load(&cache.dir, "rev", "another-binding").is_none());
        assert!(load(&cache.dir, "another-revision", "binding").is_none());
        journal.mount = Url::parse("dav://iCloud-ijklmnop@127.0.0.1:31001/").unwrap();
        journal.since += 86400;
        journal.modified = None;
        journal.refresh(&cache, alias, export).unwrap();
        assert_eq!(
            load(&cache.dir, "rev", "binding").unwrap().files,
            stored.files
        );
    }
}
