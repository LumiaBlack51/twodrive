use super::state::{FileSnapshot, KnownFolderUploadJob};
use std::{
    collections::{HashMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use twodrive_core::{Config, expand_known_folder_home};

const MAX_PROJECT_DEPTH: usize = 8;
const MAX_PROJECT_SEARCHES_PER_WINDOW: usize = 8;
const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "package.json",
    "Cargo.toml",
    "pyproject.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
];

#[derive(Debug, Default)]
pub(super) struct UploadRate {
    files: HashMap<String, VecDeque<Instant>>,
    projects: HashMap<PathBuf, VecDeque<Instant>>,
    admitted: HashMap<String, (FileSnapshot, Instant)>,
    project_cache: HashMap<PathBuf, (Option<PathBuf>, Instant)>,
    hot_projects: HashMap<PathBuf, Instant>,
    local_paths: HashMap<String, PathBuf>,
    repeat_history: HashMap<String, VecDeque<Instant>>,
    recent_repeats: VecDeque<Instant>,
    project_searches: VecDeque<Instant>,
    last_prune: Option<Instant>,
    #[cfg(test)]
    metadata_probes: usize,
}

fn expire(events: &mut VecDeque<Instant>, now: Instant, window: Duration) {
    while events
        .front()
        .is_some_and(|time| now.duration_since(*time) >= window)
    {
        events.pop_front();
    }
}

fn explicit_project_root(path: &Path, config: &Config) -> Option<PathBuf> {
    config
        .known_folders
        .upload_rate_roots
        .iter()
        .filter_map(|root| expand_known_folder_home(root).ok())
        .filter(|root| root.is_absolute() && path.starts_with(root))
        .max_by_key(|root| root.components().count())
}

impl UploadRate {
    pub(super) fn acquire(
        &mut self,
        job: &KnownFolderUploadJob,
        changed: bool,
        config: &Config,
        window: Duration,
        now: Instant,
    ) -> bool {
        // First uploads do not consume churn budgets. An admitted generation's
        // network retries likewise do not count as additional source changes.
        if !changed
            || (config.known_folders.max_file_uploads_per_window == 0
                && config.known_folders.max_project_uploads_per_window == 0)
        {
            return true;
        }
        if self
            .last_prune
            .is_none_or(|last| now.duration_since(last) >= window)
        {
            self.files.retain(|_, events| {
                events
                    .back()
                    .is_some_and(|last| now.duration_since(*last) < window)
            });
            self.projects.retain(|_, events| {
                events
                    .back()
                    .is_some_and(|last| now.duration_since(*last) < window)
            });
            self.admitted
                .retain(|_, (_, admitted)| now.duration_since(*admitted) < window);
            self.project_cache
                .retain(|_, (_, checked)| now.duration_since(*checked) < window);
            self.hot_projects
                .retain(|_, detected| now.duration_since(*detected) < window);
            self.local_paths
                .retain(|file, _| self.files.contains_key(file));
            self.repeat_history
                .retain(|file, _| self.files.contains_key(file));
            self.last_prune = Some(now);
        }
        if self
            .admitted
            .get(&job.remote_path)
            .is_some_and(|(snapshot, _)| snapshot == &job.snapshot)
        {
            return true;
        }
        let events = self.files.entry(job.remote_path.clone()).or_default();
        expire(events, now, window);
        let repeated = !events.is_empty();
        let file_max = config.known_folders.max_file_uploads_per_window;
        let file_hot = file_max > 0 && events.len() >= file_max as usize;
        self.local_paths
            .insert(job.remote_path.clone(), job.local_path.clone());
        expire(&mut self.recent_repeats, now, window);
        let project_max = config.known_folders.max_project_uploads_per_window;
        let aggregate_hot = project_max > 0 && self.recent_repeats.len() >= project_max as usize;
        // An ordinary first edit of each file never consumes the project
        // budget. The project counter starts with repeated changes in-window.
        let mut project = if repeated && project_max > 0 {
            if let Some(explicit) = explicit_project_root(&job.local_path, config) {
                // A cold explicit child is still a separate identity; an
                // already-hot ancestor must not claim its events or quota.
                self.hot_projects
                    .contains_key(&explicit)
                    .then_some(explicit)
            } else {
                self.hot_projects
                    .keys()
                    .filter(|root| job.local_path.starts_with(root))
                    .max_by_key(|root| root.components().count())
                    .cloned()
            }
        } else {
            None
        };
        // Normal events only update in-memory counters. Metadata discovery is
        // allowed solely after a file or aggregate churn threshold is reached.
        if repeated && project_max > 0 && (file_hot || aggregate_hot) && project.is_none() {
            project = self.project_root(&job.local_path, config, window, now);
            if let Some(root) = &project {
                let mut history = self
                    .repeat_history
                    .iter()
                    .filter(|(file, _)| {
                        self.local_paths.get(*file).is_some_and(|path| {
                            path.starts_with(root)
                                && explicit_project_root(path, config)
                                    .is_none_or(|owner| owner == *root)
                        })
                    })
                    .flat_map(|(_, times)| times.iter().copied())
                    .filter(|time| now.duration_since(*time) < window)
                    .collect::<Vec<_>>();
                history.sort_unstable();
                self.projects.insert(root.clone(), history.into());
                self.hot_projects.insert(root.clone(), now);
            }
        }
        if file_hot {
            return false;
        }
        if let Some(project) = &project {
            let events = self.projects.entry(project.clone()).or_default();
            expire(events, now, window);
            if events.len() >= config.known_folders.max_project_uploads_per_window as usize {
                return false;
            }
        }
        let events = self.files.get_mut(&job.remote_path).unwrap();
        // With the file limit disabled only one timestamp is needed to detect
        // repeated changes; the project log still has its independent bound.
        if file_max == 0 {
            events.clear();
        }
        events.push_back(now);
        if repeated {
            let history = self
                .repeat_history
                .entry(job.remote_path.clone())
                .or_default();
            expire(history, now, window);
            history.push_back(now);
            self.recent_repeats.push_back(now);
            while self.recent_repeats.len() > project_max.max(1) as usize {
                self.recent_repeats.pop_front();
            }
        }
        if let Some(project) = project {
            self.projects.get_mut(&project).unwrap().push_back(now);
        }
        self.admitted
            .insert(job.remote_path.clone(), (job.snapshot.clone(), now));
        true
    }

    fn project_root(
        &mut self,
        path: &Path,
        config: &Config,
        window: Duration,
        now: Instant,
    ) -> Option<PathBuf> {
        let parent = path.parent()?;
        if let Some((root, checked)) = self.project_cache.get(parent)
            && now.duration_since(*checked) < window
        {
            return root.clone();
        }
        let explicit = explicit_project_root(path, config);
        if explicit.is_some() {
            self.project_cache
                .insert(parent.to_path_buf(), (explicit.clone(), now));
            return explicit;
        }
        let source = config
            .known_folders
            .folders
            .iter()
            .filter_map(|folder| expand_known_folder_home(&folder.local).ok())
            .filter(|root| path.starts_with(root))
            .max_by_key(|root| root.components().count());
        expire(&mut self.project_searches, now, window);
        if source.is_some() {
            if self.project_searches.len() >= MAX_PROJECT_SEARCHES_PER_WINDOW {
                self.project_cache.insert(parent.to_path_buf(), (None, now));
                return None;
            }
            self.project_searches.push_back(now);
        }
        let found = source.and_then(|source| {
            for directory in parent.ancestors().take(MAX_PROJECT_DEPTH) {
                if !directory.starts_with(&source) {
                    break;
                }
                for marker in PROJECT_MARKERS {
                    #[cfg(test)]
                    {
                        self.metadata_probes += 1;
                    }
                    if fs::symlink_metadata(directory.join(marker)).is_ok_and(|metadata| {
                        metadata.is_file() || (*marker == ".git" && metadata.is_dir())
                    }) {
                        return Some(directory.to_path_buf());
                    }
                }
            }
            None
        });
        self.project_cache
            .insert(parent.to_path_buf(), (found.clone(), now));
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(name: &str, generation: u64) -> KnownFolderUploadJob {
        KnownFolderUploadJob {
            local_path: format!("/source/project/{name}").into(),
            remote_path: format!("/Code/project/{name}"),
            snapshot: FileSnapshot {
                size: generation,
                modified_unix: 1,
                modified_nanos: generation as u32,
            },
        }
    }

    #[test]
    fn sliding_file_and_project_budgets_only_limit_repeated_changes() {
        let mut config = Config::default();
        config.known_folders.max_file_uploads_per_window = 2;
        config.known_folders.max_project_uploads_per_window = 3;
        config.known_folders.upload_rate_roots = vec!["/source/project".into()];
        let window = Duration::from_secs(20);
        let now = Instant::now();
        let mut rate = UploadRate::default();
        for name in ["a", "b", "c"] {
            assert!(rate.acquire(&job(name, 1), true, &config, window, now));
            assert!(rate.acquire(
                &job(name, 2),
                true,
                &config,
                window,
                now + Duration::from_secs(19)
            ));
            assert!(!rate.acquire(
                &job(name, 3),
                true,
                &config,
                window,
                now + Duration::from_secs(19)
            ));
        }
        assert!(rate.acquire(
            &job("d", 1),
            true,
            &config,
            window,
            now + Duration::from_secs(19)
        ));
        assert!(!rate.acquire(
            &job("d", 2),
            true,
            &config,
            window,
            now + Duration::from_secs(19)
        ));
        // A failed request for the same admitted generation is not a new edit.
        assert!(rate.acquire(
            &job("a", 2),
            true,
            &config,
            window,
            now + Duration::from_secs(19)
        ));
        // At t=20 only the t=0 file slot expires. The project slots at t=19
        // remain occupied; a fixed-window reset would incorrectly admit this.
        assert!(!rate.acquire(&job("d", 2), true, &config, window, now + window));
        assert!(rate.acquire(
            &job("d", 2),
            true,
            &config,
            window,
            now + Duration::from_secs(39)
        ));
    }

    #[test]
    fn stable_bulk_uploads_and_first_edits_do_not_consume_project_budget() {
        let mut config = Config::default();
        config.known_folders.max_file_uploads_per_window = 1;
        config.known_folders.max_project_uploads_per_window = 1;
        config.known_folders.upload_rate_roots = vec!["/source/project".into()];
        let now = Instant::now();
        let window = Duration::from_secs(20);
        let mut rate = UploadRate::default();
        for index in 0..1000 {
            assert!(rate.acquire(&job(&index.to_string(), 1), false, &config, window, now));
            assert!(rate.acquire(&job(&index.to_string(), 2), true, &config, window, now));
        }
        assert_eq!(rate.metadata_probes, 0);
        config.known_folders.max_file_uploads_per_window = 0;
        config.known_folders.max_project_uploads_per_window = 0;
        for index in 0..1000 {
            assert!(rate.acquire(&job("same", index), true, &config, window, now));
        }
    }

    #[test]
    fn project_detection_is_bounded_and_caches_positive_and_negative_results() {
        let (root, _) = crate::known_folders::tests::test_paths("project-detection");
        let source = root.join("source");
        let app = source.join("course/app");
        let parent = app.join("src/nested");
        fs::create_dir_all(&parent).unwrap();
        fs::write(app.join("package.json"), b"{}").unwrap();
        let mut config = Config::default();
        config.known_folders.folders[0].local = source.to_string_lossy().into_owned();
        let mut rate = UploadRate::default();
        let now = Instant::now();
        let window = Duration::from_secs(20);
        assert_eq!(
            rate.project_root(&parent.join("a.js"), &config, window, now),
            Some(app)
        );
        let first = rate.metadata_probes;
        assert!(first <= MAX_PROJECT_DEPTH * PROJECT_MARKERS.len());
        for index in 0..1000 {
            rate.project_root(&parent.join(format!("{index}.js")), &config, window, now);
        }
        assert_eq!(rate.metadata_probes, first);
        let plain = source.join("plain");
        fs::create_dir_all(&plain).unwrap();
        assert_eq!(
            rate.project_root(&plain.join("a"), &config, window, now),
            None
        );
        let misses = rate.metadata_probes;
        assert_eq!(
            rate.project_root(&plain.join("b"), &config, window, now),
            None
        );
        assert_eq!(rate.metadata_probes, misses);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_search_only_runs_after_a_churn_threshold_is_reached() {
        let (root, _) = crate::known_folders::tests::test_paths("hot-only-project-search");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("Cargo.toml"), b"project marker").unwrap();
        let mut config = Config::default();
        config.known_folders.folders[0].local = root.to_string_lossy().into_owned();
        let mut rate = UploadRate::default();
        let now = Instant::now();
        let window = Duration::from_secs(20);
        let candidate = |generation| KnownFolderUploadJob {
            local_path: root.join("main.rs"),
            ..job("main.rs", generation)
        };
        for generation in 0..25 {
            assert!(rate.acquire(&candidate(generation), true, &config, window, now));
        }
        assert_eq!(rate.metadata_probes, 0);
        assert!(!rate.acquire(&candidate(25), true, &config, window, now));
        assert!(rate.metadata_probes > 0);
        assert!(rate.metadata_probes <= MAX_PROJECT_DEPTH * PROJECT_MARKERS.len());
        let probes = rate.metadata_probes;
        assert!(!rate.acquire(&candidate(26), true, &config, window, now));
        assert_eq!(rate.metadata_probes, probes);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_discovery_has_a_global_metadata_budget_during_churn() {
        let (root, _) = crate::known_folders::tests::test_paths("project-search-budget");
        fs::create_dir_all(&root).unwrap();
        let mut config = Config::default();
        config.known_folders.folders[0].local = root.to_string_lossy().into_owned();
        let mut rate = UploadRate::default();
        let now = Instant::now();
        for index in 0..1000 {
            rate.project_root(
                &root.join(format!("unknown-{index}/file")),
                &config,
                Duration::from_secs(20),
                now,
            );
        }
        assert_eq!(rate.project_searches.len(), MAX_PROJECT_SEARCHES_PER_WINDOW);
        assert!(
            rate.metadata_probes
                <= MAX_PROJECT_SEARCHES_PER_WINDOW * MAX_PROJECT_DEPTH * PROJECT_MARKERS.len()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_nested_projects_keep_separate_budgets_in_either_discovery_order() {
        let mut config = Config::default();
        config.known_folders.max_file_uploads_per_window = 0;
        config.known_folders.max_project_uploads_per_window = 1;
        config.known_folders.upload_rate_roots =
            vec!["/source/project".into(), "/source/project/nested".into()];
        let now = Instant::now();
        let window = Duration::from_secs(20);
        for order in [["outer", "nested/inner"], ["nested/inner", "outer"]] {
            let mut rate = UploadRate::default();
            for name in order {
                assert!(rate.acquire(&job(name, 1), true, &config, window, now));
                assert!(
                    rate.acquire(&job(name, 2), true, &config, window, now),
                    "another configured root must not consume {name}'s budget"
                );
                assert!(!rate.acquire(&job(name, 3), true, &config, window, now));
            }
            assert_eq!(rate.metadata_probes, 0);
        }
    }
}
