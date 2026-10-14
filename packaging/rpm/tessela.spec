# tessela: the runner for tessela projects (`tessela run <dir>`), with the
# GPUI renderer. Builds offline from two sources made by make-sources.sh:
# the git tree and every crate in Cargo.lock (cargo vendor).
#
#   packaging/rpm/make-sources.sh ~/rpmbuild/SOURCES
#   rpmbuild -ba packaging/rpm/tessela.spec

%bcond check 1

Name:           tessela
Version:        0.1.0
Release:        1%{?dist}
Summary:        Desktop surfaces written as sandboxed TSX/Luau projects

# tessela itself is MIT OR Apache-2.0. The binary statically links the
# vendored crates; their licenses, from %%cargo_license_summary (run on the
# workspace, so a superset of what tessela-cli links):
SourceLicense:  MIT OR Apache-2.0
License:        (MIT OR Apache-2.0) AND %{shrink:
                ((Apache-2.0 OR MIT) AND BSD-3-Clause) AND
                ((MIT OR Apache-2.0) AND Unicode-3.0) AND
                (0BSD OR MIT OR Apache-2.0) AND
                Apache-2.0 AND
                (Apache-2.0 OR BSL-1.0) AND
                (Apache-2.0 OR GPL-2.0-only) AND
                (Apache-2.0 OR MIT) AND
                (Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT) AND
                (Apache-2.0 WITH LLVM-exception OR BSL-1.0) AND
                BSD-2-Clause AND
                (BSD-2-Clause OR Apache-2.0 OR MIT) AND
                (BSD-2-Clause OR MIT OR Apache-2.0) AND
                BSD-3-Clause AND
                (BSD-3-Clause OR Apache-2.0) AND
                CC0-1.0 AND
                (CC0-1.0 OR MIT-0 OR Apache-2.0) AND
                ISC AND
                MIT AND
                (MIT OR Apache-2.0 OR LGPL-2.1-or-later) AND
                (MIT OR Apache-2.0 OR Zlib) AND
                (MIT OR Zlib OR Apache-2.0) AND
                MPL-2.0 AND
                Unicode-3.0 AND
                (Unlicense OR MIT) AND
                Zlib AND
                (Zlib OR Apache-2.0 OR MIT) AND
                bzip2-1.0.6
                }
# Provisional: the project has no public home yet (see docs/adr/0008).
URL:            https://github.com/FulecoRafa/tessela
Source0:        %{name}-%{version}.tar.xz
Source1:        %{name}-%{version}-vendor.tar.xz

ExclusiveArch:  %{rust_arches}

BuildRequires:  cargo-rpm-macros >= 26
BuildRequires:  rust >= 1.85
BuildRequires:  cargo
# mlua builds Luau (C++), rquickjs builds QuickJS (C).
BuildRequires:  gcc
BuildRequires:  gcc-c++
BuildRequires:  pkgconfig
BuildRequires:  pkgconfig(fontconfig)
BuildRequires:  pkgconfig(freetype2)
BuildRequires:  pkgconfig(xcb)
BuildRequires:  pkgconfig(xkbcommon)
BuildRequires:  pkgconfig(xkbcommon-x11)

# Linked libraries (fontconfig, freetype, xcb, xkbcommon) are found by rpm's
# dependency generator. These are dlopen'd at runtime, so it cannot see them
# (rpmlint flags them as explicit-lib-dependency; that is expected here):
Requires:       libwayland-client%{?_isa}
Requires:       libwayland-egl%{?_isa}
Requires:       vulkan-loader%{?_isa}
# OpenGL ES fallback (wgpu) when there is no Vulkan driver.
Requires:       libglvnd-egl%{?_isa}
# A Vulkan driver; NVIDIA users get theirs from the proprietary driver.
Recommends:     mesa-vulkan-drivers%{?_isa}
Recommends:     mesa-dri-drivers%{?_isa}
# Something to render text with when a project names no font, or a missing one.
Requires:       fontconfig
Recommends:     default-fonts-core-sans
# Optional tools: `tessela run --nested` starts sway; `tessela devtools shot` uses grim.
Suggests:       sway
Suggests:       grim

%description
tessela runs desktop surfaces (bars, panels, launchers) written as sandboxed
TSX or Luau projects: a tessela.toml lists the surfaces, their capabilities,
the services (plugins) the project ships and its theme. `tessela run <dir>`
renders them as Wayland layer-shell surfaces with GPUI and keeps the plugins
running.

%prep
%autosetup -n %{name}-%{version} -a1 -p1
%cargo_prep -v cargo-vendor

%build
%{cargo_build} -p tessela-cli
%{cargo_license_summary}
%{cargo_license} > LICENSE.dependencies
%cargo_vendor_manifest

%install
install -Dpm0755 target/rpm/tessela %{buildroot}%{_bindir}/tessela

%if %{with check}
%check
# The whole workspace's tests run headless: the GPUI crate's tests only build
# views, they open no window, so no Wayland compositor or GPU is needed.
%{cargo_test} --workspace
%endif

%files
%license LICENSE-MIT LICENSE-APACHE
%license LICENSE.dependencies
%license cargo-vendor.txt
%doc README.md
%{_bindir}/tessela

%changelog
* Tue Oct 13 2026 Fuleco <fuleco.ra.pha@gmail.com> - 0.1.0-1
- First package
