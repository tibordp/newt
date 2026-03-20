Name:           newt-fm
Version:        %{_newt_version}
Release:        1%{?dist}
Summary:        Dual-pane file manager
License:        GPL-2.0-only
URL:            https://github.com/tibordp/newt

%define debug_package %{nil}

Requires:       webkit2gtk4.1
Requires:       gtk3
Requires:       libappindicator-gtk3

%description
Newt is a keyboard-centric dual-pane file manager built with
Tauri, featuring SSH remoting and virtual filesystem support.

%install
make -C %{_newt_srcdir} install DESTDIR=%{buildroot} PREFIX=/usr BINARY=%{_newt_binary} AGENT_DIR=%{_newt_agent_dir}

%files
/usr/bin/newt
/usr/share/newt/agents/
/usr/share/applications/newt.desktop
/usr/share/icons/hicolor/32x32/apps/newt.png
/usr/share/icons/hicolor/128x128/apps/newt.png
/usr/share/icons/hicolor/256x256/apps/newt.png
