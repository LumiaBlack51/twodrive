# Debian packaging policy and stable migration

Effective 2026-10-15, on both `main` and `dev`, every TwoDrive `.deb` distributed
as a release must be built through `dpkg-buildpackage` and debhelper using the
tracked `debian/` packaging. `debian/control` and `debian/changelog` are the
metadata authorities. Direct `dpkg-deb --build`, a second hand-written control
file, and copying prebuilt executables into a manually assembled release `.deb`
are not accepted release paths. `dpkg-deb` remains useful for inspecting packages.
This requirement applies to future Development `.deb` packages as well as stable
ones; it does not convert Windows or portable tar archives into Debian packages.

自 2026-10-15 起，`main` 与 `dev` 发布的所有 TwoDrive `.deb` 必须通过
`dpkg-buildpackage` 和 debhelper、使用版本控制中的 `debian/` 生成。
禁止用手工 `dpkg-deb --build` 或另一份 control 文件替代发布构建。
源码、二进制、调试符号及清单应一起保存并校验；不得把上游项目分发包称为
已被 Debian 官方仓库接收。Windows 和便携 tar 的格式不受此条改变。

## Build and release contract

1. Prepare immutable source inputs and the locked dependency closure before the
   Debian build. Required `debian/rules` targets must build and test offline.
   Declare compiler and native build dependencies. Do not silently bypass
   dependency checks, tests, Lintian failures, or missing output files.
2. Run `dpkg-buildpackage` with debhelper; retain `.dsc`, original/component
   tarballs, `.debian.tar.xz`, binary/debug packages, `.buildinfo` and `.changes`.
   Verify every referenced size and SHA-256 before releasing. Unsigned project
   artifacts are marked as such; a Debian archive upload needs proper signatures.
3. Check package dependencies, payload, ownership, conffiles, manuals, and
   licenses, plus Lintian. Exercise installation, upgrade from the prior release,
   removal/purge and installed application smoke tests in a disposable system.
4. Preserve account/config paths, tokens, SQLite schema and recovery state,
   content cache, pending uploads/deletes, CLI names, FUSE behavior and desktop
   entry points. Maintainer scripts must not read or migrate user home data,
   sign in to cloud accounts, clear recovery queues, or start/enable user mounts.
   Restart an existing production service only during a separately authorized
   local upgrade after checking active writes and creating a private backup.
5. Keep policy and packaging CI on both GitHub branches. `main` releases use
   stable application sources; Development features must not enter a stable
   release as a side effect of packaging work.

## Stable 0.2.14-2

The application Rust sources, Cargo.lock, Python desktop helper logic, service and
.desktop files are unchanged from stable `main` 7eb7f3d. This is a Debian
packaging revision, not a sync-engine feature release; the CLI still reports
0.2.14. debhelper may normalize Python shebangs to /usr/bin/python3. The native SQLite dependency is linked from the distribution and checked
with the existing database/recovery tests. Generated ELF/Python dependencies
replace the incomplete hand-written runtime list. User service enablement remains
opt-in; installing/removing the package does not stop or start user mounts.

The upstream preparation helper supplies a separate `orig-vendor` component,
including the locked Rust sources and their original licenses. Debian compilation
uses an empty Cargo home inside the source tree and requires no network/cache.
Third-party attribution/license files are also collected in the binary docs.
This retains the locked upstream dependency versions; it does not claim a full
Debian Rust dependency/copyright audit or acceptance into the Debian archive.
Official inclusion additionally needs distribution-packaged Rust dependencies,
review of remaining native convenience copies, licensing review, sponsorship,
and clean Debian distribution build/autopkgtest/piuparts validation.

See [build instructions](../debian/README.source) and
[stable incident and verification record](https://github.com/LumiaBlack51/twodrive/blob/main/doc/incidents.md).
Validation and delivery results are added below only after execution.

## Development 0.1.6

Development uses the same complete source-package preparation and debhelper
build path as main. The GNOME package version is `0.2.14+dev0.1.6-1`; its
separate upstream version prevents reusing a stable original tarball for different
Development sources. The GNOME CLI version remains 0.2.14. The Flutter desktop
is supplied as portable Linux/Windows bundles and an unsigned test MSIX.

The build retains a locked vendor component and third-party license texts,
uses a fresh Cargo home offline, checks all source/binary manifests, and tests
upgrade, removal and purge with synthetic account/cache/recovery sentinels.
Ubuntu CI explicitly records its newer rustup compiler dependency-check bypass;
this upstream distribution is not an official Debian archive upload. Normal
Debian builds must satisfy all declared Build-Depends. See the
[0.1.6 release record](development-0.1.6.md).

[incident and verification record](incidents.md#td-20261015-11stable-main-debian-打包流程与用户安装兼容性迁移).
Final validation completed: 171 default Rust tests, 25 Nautilus, 4 Settings,
6 release-guard tests, strict Clippy/formatting, seven explicit FUSE regressions,
and the final CI package's mock-daemon smoke passed. The actual prior release
fails the libc6 dependency regression; the replacement passes. Eleven runtime
integration files preserve content/permissions with Python shebang normalization.

The final source/binary CI passed Debian-profile Lintian with no errors; its only
warning is that the installed checker knows an older Standards-Version. Narrow
source overrides cover hand-authored HTML and Windows text INPUT linker scripts.
All .dsc/.changes/.buildinfo member hashes/sizes were verified after downloading.
The downloaded .dsc rebuilt and ran its tests in a fresh container without any
network or Cargo cache, preserving all 16043 vendored file checksums. The normal
compiler Build-Depends bypass in this upstream Ubuntu/rustup environment remains
explicit; it is not a clean Debian archive build.

A disposable Ubuntu system upgraded the old release, then removed/reinstalled/
purged the new package without changing any synthetic account, cache or recovery
state hash. A private copy of the laptop database remained logically unchanged
under the new SQLite. The real laptop was updated from the final CI deb while
preserving its existing per-user layout and custom service. Its daemon matches
the release package; config/credentials, database integrity, mounted access and
the historical rejected deletion were checked. A private recovery backup exists.
No new real OneDrive write/read/delete end-to-end test was performed.

Main implementation PR #24 merged as e003ec2eb4438677b58abcaa928b8cfa30018e42;
release source/tag v0.2.14-2 is 408370942c39473688813935e46579fa84e9bbe8,
with the same Git tree. Dev policy PR #25 merged as
4942f2ded44cdb805c397c90b932ca06c23dae0e. Active GitHub ruleset 25148176
requires `Debian packaging policy` on both main and dev. [Release 0.2.14-2](https://github.com/LumiaBlack51/twodrive/releases/tag/v0.2.14-2)
was published as stable Latest at 2026-10-15 17:16:54 +08:00. All 11 assets,
source tag and hashes passed the publication guard before and after publication.
See [machine-readable verification](debian-packaging-validation-2026-10-15.json)
and the incident log. The CLI continues reporting 0.2.14 because -2 is the
packaging revision. The laptop remains a per-user installation updated from
this standard package, rather than a newly registered system-wide dpkg install.
