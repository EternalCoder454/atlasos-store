# Atlas Store for AtlasOS.

# No debuginfo subpackage: the Rust flags below keep symbols (debuginfo=2,
# strip=none) and the binary is shipped as built.
%global debug_package %{nil}

Name:           atlas-store
Version:        0.1.0
Release:        1%{?dist}
Summary:        Atlas Store, the app store of AtlasOS
License:        MIT
URL:            https://github.com/EternalCoder454/atlasos-store
Source0:        atlas-store-%{version}.tar.gz

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
# libflatpak, for installs, removals and updates (atlas-framework-flatpak)
BuildRequires:  pkgconfig(flatpak)
# QML modules qmlcachegen resolves at build time (not linked). atlas-ui comes
# from atlas-framework, which is in no repository: install its RPMs first
# (build-rpm.sh does, given ATLAS_LOCAL_RPMS).
BuildRequires:  kf6-kirigami-devel
BuildRequires:  atlas-ui >= 1.4.0

Requires:       kf6-kirigami
# Atlas.Ui, the shared look (atlas-framework); 1.4.0 for AtlasSidebar,
# AtlasAppCard, AtlasScreenshotCarousel and AtlasInstallButton
Requires:       atlas-ui >= 1.4.0
Requires:       kf6-qqc2-desktop-style
Requires:       qt6-qtdeclarative
# the app icon and Breeze's icons are SVG
Requires:       qt6-qtsvg
# the system installation, its polkit rules and the remotes it reads
Requires:       flatpak

%description
Atlas Store is the app store of AtlasOS. Browse and search the apps on Flathub
and your other Flatpak sources, see their screenshots, what they can access and
who made them, and install, update and remove them. It opens flatpak: links,
appstream: links and .flatpakref, .flatpakrepo and .flatpak files.

%prep
%autosetup -n atlas-store-%{version}

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
%global _vpath_srcdir apps/atlas-store
%cmake -G Ninja -DCMAKE_BUILD_TYPE=Release
%cmake_build

%install
%cmake_install
# AtlasOS needs its app store: dnf refuses to remove it.
install -Dpm0644 apps/atlas-store/data/dnf/protected.d/atlas-store.conf \
    %{buildroot}%{_sysconfdir}/dnf/protected.d/atlas-store.conf

%check
# No path into the build tree (checked as well as set: see %%build).
# grep: 0 = found, 1 = not found, anything else (no binary) fails too.
rc=0
grep -qF "%{_builddir}" %{buildroot}%{_bindir}/atlas-store || rc=$?
if [ "$rc" != 1 ]; then
    echo "atlas-store holds the build path %{_builddir} (grep status $rc)" >&2
    exit 1
fi
desktop-file-validate %{buildroot}%{_datadir}/applications/net.eterneon.atlas.store.desktop
appstream-util validate-relax --nonet \
    %{buildroot}%{_datadir}/metainfo/net.eterneon.atlas.store.metainfo.xml

%files
%license LICENSE
%{_bindir}/atlas-store
%{_datadir}/applications/net.eterneon.atlas.store.desktop
%{_datadir}/metainfo/net.eterneon.atlas.store.metainfo.xml
%{_datadir}/icons/hicolor/scalable/apps/net.eterneon.atlas.store.svg
%config(noreplace) %{_sysconfdir}/dnf/protected.d/atlas-store.conf

%changelog
* Mon Oct 05 2026 EternalHell <77252745+EternalCoder454@users.noreply.github.com> - 0.1.0-1
- First package
