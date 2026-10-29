# Linux / Zorin adaptation for PR47

This branch starts at PR47 head `f4e07fe75bfb004e8d4591d1f9f6eff11391c4eb` (production code matches `0346913`). It incorporates the subsequent PR47 Baidu Content-MD5 and default-editable changes through `730a824967378e0f386708e7d522e6c5c2dff421`. It adds Linux FUSE integration while retaining the shared UI, provider authentication, default editable cloud accounts and storage identity checks. It does not replace the stable daemon.

## Behavior

- Retain simultaneous accounts and their mounts. Selecting an online account uses revision-bound `select`, preserves its last directory, and never implicitly authenticates.
- Keep native FUSE and its durable conditional-upload cache. The private Provider bridge uses Linux mount ownership/cleanup and platform-specific persistent caches for manual-edit adapters. Cloud/WebDAV connections are editable by default, including legacy profiles with `writable:false`; no local mode or password is required. Old identity, revision, credentials and paths remain unchanged. Paired devices still respect owner-granted rights.
- Coalesce same-version file downloads without holding the shared cache lock over remote I/O. Reuse one live path lookup within each GET, and validate again on the next request. Complete contents remain verified before response headers, so cold reads can still show a preparation pause in the file manager.
- Keep active parent directories hot during child probes with a bounded 64-directory LRU (100,000 entries total). Linux read-only bridge navigation retains native directory listings for 30 seconds; editable and Windows mounts retain the 5-second policy. Display snapshot age remains capped at five minutes and real GET validation is unchanged.
- Linux iCloud metadata uses short-lived private encrypted config views so a directory lookup does not queue behind a whole-file content operation. Auth binding, account identity, nonblocking compare-and-save and cleanup guards preserve the canonical session; metadata isolation also applies to editable Linux connections; actual mutation locking and Windows query behavior remain unchanged.
- Answer filesystem capacity from a bounded display snapshot, with one separate background worker per bridge. Cold or expired quota never blocks Statfs/GIO classification; successful values refresh after five minutes and expire after thirty minutes, with a thirty-second failure backoff. Native capacity may remain unknown until its next query after the initial background result; account capacity cards are separate and unchanged.
- Bound directory display snapshots, coalesce refreshes, reject failed listings before successful DAV headers, and keep HEAD metadata-only. File downloads still validate live object/version/size. Editable listings retain the first observed version; every mutation compares it with live metadata. Any mutation response invalidates display snapshots, and an in-flight older listing cannot refill them.
- Coalesce iCloud rename preflight checks using a fresh parent listing. Linux same-directory ordinary-file rename now shares one authenticated component session for fresh preflight, server move and fresh confirmation; authorization, observed version/type, no-overwrite, confirmation and parent-version guards remain. The user reports noticeably faster live rename after deployment; the latest successful provider transaction still took about 14.4 seconds, including about 6.7 seconds for final confirmation. The record is not uniquely correlated to that user action and is not a controlled before/after benchmark; remaining latency is unresolved.
- Validate Baidu content digests in both strict hexadecimal and standard Base64 formats; invalid or mismatched digests still fail.
- Transparent semantic cloud icons, bundled licensed Chinese/Latin fonts, responsive settings transitions, readable noncolliding mount names and Linux metadata-only preflight.

## Build and isolated checks

Use the prerequisites in [the desktop guide](linux-dav-desktop.md). Keep the full Linux bundle and its pinned Rclone component. No account state or external OAuth JSON is bundled.

```bash
cargo test --locked --manifest-path experiments/twodrive-dev/Cargo.toml --features mount --lib --bin twodrive-dav-engine --test writable_mount
cargo clippy --locked --manifest-path experiments/twodrive-dev/Cargo.toml --features mount --all-targets --release -- -D warnings
cargo build --locked --manifest-path experiments/twodrive-dev/Cargo.toml --features mount --release --bins
(cd apps/dav && flutter pub get --enforce-lockfile && flutter analyze && flutter test && flutter build linux --release)
python3 scripts/install-rclone-component.py --platform linux-amd64 --destination dist/rclone
TWODRIVE_RCLONE_BIN="$PWD/dist/rclone/rclone" python3 scripts/test-linux-readonly-mount.py --engine experiments/twodrive-dev/target/release/twodrive-dav-engine --peer experiments/twodrive-dev/target/release/twodrive-dev
TWODRIVE_TEST_RCLONE="$PWD/dist/rclone/rclone" cargo test --locked --manifest-path experiments/twodrive-dev/Cargo.toml --features mount --test bridge_fuse_linux -- --ignored
TWODRIVE_TEST_RCLONE="$PWD/dist/rclone/rclone" cargo test --locked --manifest-path experiments/twodrive-dev/Cargo.toml --features mount --lib native_statfs_and_gio_return_while_cloud_quota_is_stalled -- --ignored
python3 scripts/test-linux-locked-mount.py --engine experiments/twodrive-dev/target/release/twodrive-dav-engine --peer experiments/twodrive-dev/target/release/twodrive-dev
```

Tests use disposable synthetic accounts. Native mount tests require accessible `/dev/fuse` and `fusermount3`. Do not point test helpers at real account state.

| Locally executed validation | Result |
|---|---|
| Flutter model/widget tests and analyzer | 93 passed |
| Rust library / engine / editable bridge | 117 / 26 / 8 passed; 8 default ignored excluded |
| Strict release Clippy and Linux release binaries | Passed |
| Actual editable bridge FUSE | 1 explicit test passed |
| Actual read-only FUSE Statfs/GIO with held cloud quota, then capacity recovery | 1 explicit test passed |
| Dual owner-granted read-only peer mounts, copy to local and navigation with a paused upstream | 8 groups passed |
| Default editable restart, pending bytes and recovery | 4 groups passed |
| Native saved-session restart | 6 groups passed |
| Concurrent online switching/control | 9 groups passed |
| Synthetic local QUIC sharing | 7 groups passed |
| Private GNOME/Xvfb window transitions | Earlier verified; native runner unchanged in this update |

These results do not establish hosted CI success, a Windows rebuild, real-cloud editing acceptance, or full Nautilus preview performance. iCloud/Baidu manual edits retain the documented check-to-publication race and do not gain atomic conditional-upload/backup capability. See incidents TD-20261029-33/34/35/36/37/38/39/40/41.

## Delivery

Published in the separate Linux branch via [PR #50](https://github.com/LumiaBlack51/twodrive/pull/50), ready for review. PR47 through `730a824` is synchronized in [Linux commit 79a4f26](https://github.com/DavidXile/twodrive/commit/79a4f26e6f5deb728ec7e3a89e49b4d86a65d3e4). The Linux GUI and three backend programs are rebuilt, and the four original mounts restore in read-write mode without changing account profiles or saved identities. Icons, desktop preferences, simultaneous-account switching and earlier metadata fixes remain.

The user previously reported slow iCloud navigation despite caching. This update extends the existing directory and encrypted metadata isolation to editable connections, with isolated regression coverage; it does not promise instantaneous cold directories or cloud contents. No private file contents or real-cloud writes were used for deployment validation. See TD-20261029-38 for the cache regression and local recovery/verification boundaries.

## 隐私范围

公开分支只包含程序源码、通用文档、合成测试和可分发字体及其许可证。真实账号、邮箱、授权凭据、OAuth客户端JSON、网盘列表/容量、缓存/数据库、主机路径、截图和本机部署报告均留在本地。云盘默认可编辑，已取消本机只读模式及编辑密码；设备共享仍由共享方授权。
