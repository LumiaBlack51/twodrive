//! Metadata-only, all-or-nothing namespace planning before root registration.
//! Opaque IDs remain exact; only sibling names use the host's comparison rules.
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
};
use twodrive_core::IndexedItem;

#[derive(Clone, Debug)]
pub struct PlannedEntry {
    pub item: IndexedItem,
    pub components: Vec<String>,
}
impl PlannedEntry {
    pub fn relative_path(&self) -> PathBuf {
        self.components.iter().collect()
    }
    pub fn cloud_path(&self) -> String {
        format!("/{}", self.components.join("/"))
    }
}
#[derive(Clone, Debug)]
pub struct NamespacePlan {
    pub root_id: String,
    /// Parent directories always precede their descendants.
    pub entries: Vec<PlannedEntry>,
    pub max_depth: usize,
    pub max_full_path_utf16: usize,
    pub paths_at_least_260_utf16: usize,
}
impl NamespacePlan {
    /// Explicit ordinary-object mode. Unknown service objects and their whole
    /// subtrees remain outside this independent root. Existing owned objects
    /// can never disappear through this filter.
    pub fn build_ordinary(
        items: &[IndexedItem],
        expected_root: &str,
        root_utf16: usize,
        protected: &HashSet<String>,
        compare_names: impl FnMut(&str, &str) -> Result<Ordering, String>,
    ) -> Result<(Self, usize), String> {
        if items.is_empty() || items.len() > 1_000_000 {
            return Err("invalid catalogue planning bounds".into());
        }
        let mut ids = HashSet::new();
        let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
        let mut excluded = HashSet::new();
        for item in items {
            if item.id.is_empty() || !ids.insert(item.id.as_str()) {
                return Err("unsupported or duplicate catalogue identity".into());
            }
            if let Some(parent) = &item.parent {
                children.entry(parent).or_default().push(&item.id);
            }
            if !matches!(item.kind.as_str(), "file" | "folder") {
                excluded.insert(item.id.clone());
            }
        }
        let mut queue: VecDeque<String> = excluded.iter().cloned().collect();
        while let Some(parent) = queue.pop_front() {
            for child in children.get(parent.as_str()).into_iter().flatten() {
                if excluded.insert((*child).to_owned()) {
                    queue.push_back((*child).to_owned());
                }
            }
        }
        if excluded.contains(expected_root) || excluded.iter().any(|id| protected.contains(id)) {
            return Err(
                "unsupported namespace overlaps an owned object; local data retained".into(),
            );
        }
        let kept = items
            .iter()
            .filter(|i| !excluded.contains(&i.id))
            .cloned()
            .collect::<Vec<_>>();
        Ok((
            Self::build(&kept, expected_root, root_utf16, compare_names)?,
            excluded.len(),
        ))
    }
    /// `root_utf16` counts the intended absolute target without a trailing
    /// separator. The planner performs no root, account, file or cloud mutation.
    pub fn build(
        items: &[IndexedItem],
        expected_root: &str,
        root_utf16: usize,
        mut compare_names: impl FnMut(&str, &str) -> Result<Ordering, String>,
    ) -> Result<Self, String> {
        if items.is_empty() || items.len() > 1_000_000 || root_utf16 > 32700 {
            return Err("invalid catalogue planning bounds".into());
        }
        let mut by_id = HashMap::new();
        for item in items {
            if item.id.is_empty()
                || item.id.len() > 4096
                || item.deleted
                || !matches!(item.kind.as_str(), "file" | "folder")
                || by_id.insert(item.id.as_str(), item).is_some()
            {
                return Err("unsupported or duplicate catalogue identity".into());
            }
        }
        let root = *by_id
            .get(expected_root)
            .ok_or("verified drive root missing from catalogue")?;
        if root.kind != "folder" || root.parent.is_some() {
            return Err("catalogue root does not match the verified drive root".into());
        }
        let mut children: HashMap<&str, Vec<&IndexedItem>> = HashMap::new();
        for item in items.iter().filter(|item| item.id != root.id) {
            crate::names::validate_native_name(&item.name)?;
            if item.etag.is_empty() || item.size > i64::MAX as u64 {
                return Err("catalogue entry has no usable version or size".into());
            }
            let parent = item.parent.as_deref().ok_or("missing catalogue parent")?;
            if by_id
                .get(parent)
                .is_none_or(|parent| parent.kind != "folder")
            {
                return Err("catalogue parent is missing or not a directory".into());
            }
            children.entry(parent).or_default().push(item);
        }
        for siblings in children.values_mut() {
            let mut failure = None;
            siblings.sort_by(|a, b| match compare_names(&a.name, &b.name) {
                Ok(order) => order,
                Err(error) => {
                    failure = Some(error);
                    a.name.cmp(&b.name)
                }
            });
            if let Some(error) = failure {
                return Err(error);
            }
            for pair in siblings.windows(2) {
                if compare_names(&pair[0].name, &pair[1].name)? == Ordering::Equal {
                    return Err("catalogue has conflicting native sibling names".into());
                }
            }
        }
        let mut plan = Self {
            root_id: root.id.clone(),
            entries: Vec::new(),
            max_depth: 0,
            max_full_path_utf16: root_utf16,
            paths_at_least_260_utf16: 0,
        };
        let mut queue = VecDeque::from([(root.id.as_str(), Vec::<String>::new())]);
        let mut visited = HashSet::from([root.id.as_str()]);
        while let Some((parent, path)) = queue.pop_front() {
            for item in children.get(parent).into_iter().flatten() {
                if !visited.insert(item.id.as_str()) {
                    return Err("catalogue cycle or repeated identity".into());
                }
                let mut components = path.clone();
                components.push(item.name.clone());
                let full = root_utf16
                    + components
                        .iter()
                        .map(|name| name.encode_utf16().count() + 1)
                        .sum::<usize>();
                if components.len() > 128 || full > 32760 {
                    return Err("catalogue path exceeds native planning bounds".into());
                }
                plan.max_depth = plan.max_depth.max(components.len());
                plan.max_full_path_utf16 = plan.max_full_path_utf16.max(full);
                if full >= 260 {
                    plan.paths_at_least_260_utf16 += 1;
                }
                if item.kind == "folder" {
                    queue.push_back((item.id.as_str(), components.clone()));
                }
                plan.entries.push(PlannedEntry {
                    item: (*item).clone(),
                    components,
                });
            }
        }
        if visited.len() != items.len() {
            return Err("catalogue contains an unreachable cycle or subtree".into());
        }
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn item(id: &str, parent: Option<&str>, name: &str, folder: bool) -> IndexedItem {
        IndexedItem {
            id: id.into(),
            parent: parent.map(str::to_string),
            name: name.into(),
            kind: if folder { "folder" } else { "file" }.into(),
            size: 0,
            etag: "v1".into(),
            modified: None,
            deleted: false,
        }
    }
    fn plan(items: &[IndexedItem]) -> Result<NamespacePlan, String> {
        let root = items
            .iter()
            .find(|item| item.parent.is_none())
            .map(|item| item.id.as_str())
            .unwrap_or("verified-root-missing");
        NamespacePlan::build(items, root, 25, |a, b| {
            Ok(a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()))
        })
    }
    #[test]
    fn ordinary_scope_excludes_whole_special_subtrees_but_never_owned_data() {
        let root = item("root", None, "root", true);
        let mut special = item("special", Some("root"), "service-object", true);
        special.kind = "unsupported".into();
        let child = item("child", Some("special"), "retained.txt", false);
        let normal = item("normal", Some("root"), "normal.txt", false);
        let items = vec![root, special, child, normal];
        assert!(plan(&items).is_err(), "strict planning remains strict");
        let compare = |a: &str, b: &str| Ok(a.cmp(b));
        let (selected, count) =
            NamespacePlan::build_ordinary(&items, "root", 25, &HashSet::new(), compare).unwrap();
        assert_eq!(count, 2);
        assert_eq!(
            selected
                .entries
                .iter()
                .map(|e| e.item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["normal"]
        );
        for id in ["special", "child"] {
            assert!(
                NamespacePlan::build_ordinary(
                    &items,
                    "root",
                    25,
                    &HashSet::from([id.into()]),
                    compare
                )
                .is_err()
            );
        }
        let mut duplicate = items.clone();
        duplicate.push(items[1].clone());
        assert!(
            NamespacePlan::build_ordinary(&duplicate, "root", 25, &HashSet::new(), compare)
                .is_err()
        );
        assert!(
            NamespacePlan::build_ordinary(&items, "special", 25, &HashSet::new(), compare).is_err()
        );
    }
    #[test]
    fn planning_is_parent_first_and_rejects_cycles_or_native_collisions() {
        let root = item("exact-root", None, "root", true);
        let folder = item("exact-parent", Some("exact-root"), "folder", true);
        let child = item("exact-child", Some("exact-parent"), "中文.docx", false);
        let result = plan(&[child.clone(), root.clone(), folder.clone()]).unwrap();
        assert_eq!(result.root_id, "exact-root");
        assert_eq!(result.entries[0].item.id, "exact-parent");
        assert_eq!(result.entries[1].cloud_path(), "/folder/中文.docx");
        assert_eq!(
            result.entries[1].relative_path(),
            PathBuf::from("folder").join("中文.docx")
        );
        let collision = item("other-child", Some("exact-root"), "FOLDER", false);
        assert!(plan(&[root.clone(), folder.clone(), collision]).is_err());
        let mut cycle = folder.clone();
        cycle.parent = Some(child.id.clone());
        let mut child_folder = child.clone();
        child_folder.kind = "folder".into();
        assert!(plan(&[root.clone(), cycle, child_folder]).is_err());
        let mut alias = child;
        alias.name = "COM¹.txt".into();
        assert!(plan(&[root, folder, alias]).is_err());
    }
    #[test]
    fn root_orphans_versions_and_host_comparison_errors_fail_before_publication() {
        let root = item("root", None, "root", true);
        let child = item("a", Some("root"), "a", false);
        let mut orphan = child.clone();
        orphan.parent = Some("missing".into());
        assert!(plan(&[root.clone(), orphan]).is_err());
        let mut unversioned = child.clone();
        unversioned.etag.clear();
        assert!(plan(&[root.clone(), unversioned]).is_err());
        let other = item("b", Some("root"), "b", false);
        assert!(
            NamespacePlan::build(&[root.clone(), child.clone(), other], "root", 25, |_, _| {
                Err("host comparison unavailable".into())
            })
            .is_err()
        );
        assert!(plan(&[root.clone(), child.clone(), child]).is_err());
        let mut deep = vec![root];
        let mut parent = "root".to_string();
        for n in 0..129 {
            let id = format!("dir-{n}");
            deep.push(item(&id, Some(&parent), "folder", true));
            parent = id;
        }
        assert!(plan(&deep).is_err());
    }
}
