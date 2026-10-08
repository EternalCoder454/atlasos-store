# Telamon Store for Telamon OS.

# No debuginfo subpackage: the Rust flags below keep symbols (debuginfo=2,
# strip=none) and the binary is shipped as built.
%global debug_package %{nil}

Name:           telamon-store
Version:        0.3.0
Release:        1%{?dist}
Summary:        Telamon Store, the app store of Telamon OS
License:        MIT
URL:            https://github.com/EternalCoder454/atlasos-store
Source0:        telamon-store-%{version}.tar.gz
# Renamed from atlas-store in 0.2.0.
Obsoletes:      atlas-store < 0.2.0
Provides:       atlas-store = %{version}-%{release}

BuildRequires:  cargo
BuildRequires:  rust
# %%build_rustflags
BuildRequires:  rust-srpm-macros
BuildRequires:  gcc
BuildRequires:  gcc-c++
BuildRequires:  cmake
BuildRequires:  ninja-build
BuildRequires:  corrosion
# Cargo fetches the atlas-framework crates from GitHub.
BuildRequires:  git-core
BuildRequires:  desktop-file-utils
BuildRequires:  libappstream-glib
BuildRequires:  cmake(Qt6Core)
BuildRequires:  cmake(Qt6Gui)
BuildRequires:  cmake(Qt6Qml)
BuildRequires:  cmake(Qt6Quick)
BuildRequires:  cmake(Qt6QuickControls2)
BuildRequires:  cmake(Qt6Widgets)
BuildRequires:  cmake(Qt6QmlTools)
BuildRequires:  qt6-qtbase-devel
BuildRequires:  cmake(KF6DBusAddons)
BuildRequires:  cmake(KF6WindowSystem)
# libflatpak, for installs, removals and updates (telamon-framework-flatpak)
BuildRequires:  pkgconfig(flatpak)
# QML modules qmlcachegen resolves at build time (not linked). telamon-ui comes
# from atlas-framework, which is in no repository: install its RPMs first
# (build-rpm.sh does, given TELAMON_LOCAL_RPMS).
BuildRequires:  kf6-kirigami-devel
BuildRequires:  telamon-ui >= 2.0.0

Requires:       kf6-kirigami
# Telamon.Ui, the shared look (atlas-framework): TelamonSidebar,
# TelamonAppCard, TelamonScreenshotCarousel and TelamonInstallButton
Requires:       telamon-ui >= 2.0.0
Requires:       kf6-qqc2-desktop-style
Requires:       qt6-qtdeclarative
# the app icon and Breeze's icons are SVG
Requires:       qt6-qtsvg
# the system installation, its polkit rules and the remotes it reads
Requires:       flatpak

%description
Telamon Store is the app store of Telamon OS. Browse and search the apps on Flathub
and your other Flatpak sources, see their screenshots, what they can access and
who made them, and install, update and remove them. It opens flatpak: links,
appstream: links and .flatpakref, .flatpakrepo and .flatpak files.

%prep
%autosetup -n telamon-store-%{version}

%build
# NETWORK: cargo (Corrosion runs it with --locked) fetches crates.io and the
# pinned atlas-framework crates during %%build. That works in podman and with
# `rpmbuild` on a networked machine, not in an offline mock/Koji build.
# CARGO_HOME from the environment keeps a crate cache between builds
# (CLAUDE.md mounts one); otherwise a fresh one in the build dir.
export CARGO_HOME=${CARGO_HOME:-%{_builddir}/cargo-home}
# Fedora's Rust flags (hardening, build-id, ...), also used by Corrosion's
# cargo. The remaps keep build paths (panic locations, assert file names) out
# of the package, as atlas-framework's DESIGN.md asks of apps using its crates.
# HOST_CXXFLAGS reaches only the C++ that cargo's build scripts compile (cc-rs
# reads HOST_ when not cross-compiling; CMake ignores it). CFLAGS and CXXFLAGS
# are Fedora's plus the same remap for the C++ CMake builds, so that two
# builds of one commit give the same build ID. These flags split on spaces,
# so _topdir must have none (build-rpm.sh's hasn't).
export RUSTFLAGS="%{build_rustflags} --remap-path-prefix=$PWD=. --remap-path-prefix=$CARGO_HOME=cargo"
export HOST_CXXFLAGS="-ffile-prefix-map=$PWD=. -ffile-prefix-map=$CARGO_HOME=cargo"
export CFLAGS="%{build_cflags} -ffile-prefix-map=$PWD=."
export CXXFLAGS="%{build_cxxflags} -ffile-prefix-map=$PWD=."
export CARGO_PROFILE_RELEASE_STRIP=none
# (%%cmake honours _vpath_srcdir, not __cmake_source_dir)
%global _vpath_srcdir apps/telamon-store
%cmake -G Ninja -DCMAKE_BUILD_TYPE=Release
%cmake_build

%install
%cmake_install
# Telamon OS needs its app store: dnf refuses to remove it.
install -Dpm0644 apps/telamon-store/data/dnf/protected.d/telamon-store.conf \
    %{buildroot}%{_sysconfdir}/dnf/protected.d/telamon-store.conf

# The old names, for this release (the apps move to Telamon one by one):
# Telamon Updater's "open the Store" runs `atlas-store --app <id>` or launches
# net.eterneon.atlas.store.desktop until it has moved too. The old desktop
# file is hidden (NoDisplay) and starts the new binary; the old icon name
# resolves to the new icon.
ln -s telamon-store %{buildroot}%{_bindir}/atlas-store
install -Dpm0644 apps/telamon-store/data/net.eterneon.atlas.store.desktop \
    %{buildroot}%{_datadir}/applications/net.eterneon.atlas.store.desktop
ln -s net.eterneon.telamon.store.svg \
    %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/net.eterneon.atlas.store.svg

%check
# No path into the build tree (checked as well as set: see %%build).
# grep: 0 = found, 1 = not found, anything else (no binary) fails too.
rc=0
grep -qF "%{_builddir}" %{buildroot}%{_bindir}/telamon-store || rc=$?
if [ "$rc" != 1 ]; then
    echo "telamon-store holds the build path %{_builddir} (grep status $rc)" >&2
    exit 1
fi
desktop-file-validate %{buildroot}%{_datadir}/applications/net.eterneon.telamon.store.desktop
desktop-file-validate %{buildroot}%{_datadir}/applications/net.eterneon.atlas.store.desktop
test "$(readlink %{buildroot}%{_bindir}/atlas-store)" = telamon-store
appstream-util validate-relax --nonet \
    %{buildroot}%{_datadir}/metainfo/net.eterneon.telamon.store.metainfo.xml

%files
%license LICENSE
%{_bindir}/telamon-store
%{_bindir}/atlas-store
%{_datadir}/applications/net.eterneon.telamon.store.desktop
%{_datadir}/applications/net.eterneon.atlas.store.desktop
%{_datadir}/metainfo/net.eterneon.telamon.store.metainfo.xml
%{_datadir}/icons/hicolor/scalable/apps/net.eterneon.telamon.store.svg
%{_datadir}/icons/hicolor/scalable/apps/net.eterneon.atlas.store.svg
%config(noreplace) %{_sysconfdir}/dnf/protected.d/telamon-store.conf

%changelog
* Wed Oct 07 2026 EternalHell <77252745+EternalCoder454@users.noreply.github.com> - 0.3.0-1
- Sources: list the Flatpak sources of both installations, turn them on and off, add one from a link or a
  .flatpakrepo file (the title, address and key are shown before anything is added), and remove one (refused,
  naming the apps, while apps from it are installed).
- Updates: the app updates through Telamon Updater's engine (its lock, history and "asks for new permissions"
  check), Update All, progress, plain-word errors and "Last checked"; the settings stay in Telamon Settings.
- Home: Popular Apps, New & Updated and Editor's Picks from Flathub's API, and Popular in <Category> on each
  category page; cached, shown offline, matched against the local catalog before they are shown.
- Category pages: the two filters are now labelled chips (Verified, Free Software), not unlabelled switches.
- A source without a key is added without signature checking only after an extra confirmation.

* Wed Oct 07 2026 EternalHell <77252745+EternalCoder454@users.noreply.github.com> - 0.2.0-1
- Renamed to Telamon Store (telamon-store, net.eterneon.telamon.store), on telamon-ui 2.0.0. Obsoletes and
  provides atlas-store. For this release /usr/bin/atlas-store, a hidden net.eterneon.atlas.store.desktop and
  the old icon name stay; the Updater's lock and the Store's own lock are taken under both names; the
  catalog cache and the pending-sources journal move to their new folders on the first run.

* Mon Oct 05 2026 EternalHell <77252745+EternalCoder454@users.noreply.github.com> - 0.1.0-1
- First package
