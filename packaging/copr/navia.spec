# Built from the release tag. Needs network during the build: Cargo downloads the crates and
# vendor/gpui/fetch-gpui.sh downloads the pinned GPUI. The COPR project has network enabled.
%global app_id com.davekai.Navia
%global debug_package %{nil}

Name:           navia
Version:        0.2.0
Release:        1%{?dist}
Summary:        Music client for Navidrome
License:        GPL-3.0-or-later
URL:            https://github.com/davekai/navia
Source0:        %{url}/archive/refs/tags/v%{version}/%{name}-%{version}.tar.gz

ExclusiveArch:  x86_64 aarch64

BuildRequires:  cargo >= 1.99
BuildRequires:  rust >= 1.99
BuildRequires:  gcc
BuildRequires:  gcc-c++
BuildRequires:  clang
BuildRequires:  cmake
BuildRequires:  curl
BuildRequires:  patch
BuildRequires:  perl
BuildRequires:  pkgconfig
BuildRequires:  alsa-lib-devel
BuildRequires:  fontconfig-devel
BuildRequires:  freetype-devel
BuildRequires:  libxkbcommon-devel
BuildRequires:  libzstd-devel
BuildRequires:  opus-devel
BuildRequires:  wayland-devel
BuildRequires:  desktop-file-utils
BuildRequires:  libappstream-glib

# Loaded at run time, so the automatic dependencies miss them. Navia is Wayland-only.
Requires:       vulkan-loader
Requires:       libwayland-client
Requires:       libwayland-egl
Recommends:     pipewire-alsa
Recommends:     mesa-libEGL
Recommends:     mesa-vulkan-drivers
Requires:       hicolor-icon-theme

%description
A native music client for Navidrome. Navia keeps a copy of your library on
your computer, so browsing and search stay fast. It plays gapless, can
crossfade, and shows synced lyrics. Songs, albums and playlists can be
downloaded for listening offline. Navia runs on Wayland only.

%prep
%autosetup -n %{name}-%{version}
vendor/gpui/fetch-gpui.sh

%build
# Plain release profile, like the published builds. A tag build stamps no commit.
export RUSTFLAGS=""
export CARGO_HOME="$PWD/.cargo-home"
export CARGO_PROFILE_RELEASE_STRIP=symbols
export CARGO_PROFILE_RELEASE_DEBUG=0
export CARGO_INCREMENTAL=0
export NAVIA_PACKAGE_NAME=%{name}
export NAVIA_COMMIT=
export NAVIA_BUILD=
cargo build --release --locked -j %{_smp_build_ncpus} -p navia-gpui

%install
install -Dm 755 target/release/navia-gpui %{buildroot}%{_bindir}/%{name}
sed -e 's|@APP_NAME@|%{name}|g' -e 's|@APP_ID@|%{app_id}|g' \
    -e 's|@APP_DISPLAY_NAME@|Navia|g' -e 's|@APP_SUMMARY@|%{summary}|g' \
    packaging/linux/app.desktop.in > %{app_id}.desktop
install -Dm 644 %{app_id}.desktop %{buildroot}%{_datadir}/applications/%{app_id}.desktop
install -Dm 644 packaging/linux/%{app_id}.metainfo.xml %{buildroot}%{_metainfodir}/%{app_id}.metainfo.xml
install -Dm 644 packaging/linux/icons/app.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/%{app_id}.svg
for png in packaging/linux/icons/app-*.png; do
    size="${png##*/app-}"; size="${size%.png}"
    install -Dm 644 "$png" %{buildroot}%{_datadir}/icons/hicolor/${size}x${size}/apps/%{app_id}.png
done

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/%{app_id}.desktop
appstream-util validate-relax --nonet %{buildroot}%{_metainfodir}/%{app_id}.metainfo.xml
test "$(%{buildroot}%{_bindir}/%{name} --version)" = "%{name} %{version}"

%files
%license LICENSE
%{_bindir}/%{name}
%{_datadir}/applications/%{app_id}.desktop
%{_metainfodir}/%{app_id}.metainfo.xml
%{_datadir}/icons/hicolor/*/apps/%{app_id}.*

%changelog
* Sun Nov 22 2026 davekai - 0.2.0-1
- Navia 0.2.0

* Sun Nov 15 2026 davekai - 0.1.2-1
- Navia 0.1.2

* Sun Nov 15 2026 davekai - 0.1.1-1
- Package for COPR, built from source
