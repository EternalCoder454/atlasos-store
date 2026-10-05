#!/bin/bash
# A local, signed Flatpak remote and test installations for the Store's tests,
# so nothing ever touches a real installation or Flathub. Run it in the dev
# container (scripts/dev.sh scripts/test-remote.sh ...).
#
#   scripts/test-remote.sh build [dir]   make the remote: a runtime with an
#                                        extension, an app (version 1.0, on
#                                        stable and beta) and an add-on for it
#   scripts/test-remote.sh bump [dir]    publish the app's version 1.1, which
#                                        asks for more permissions
#   scripts/test-remote.sh check [dir]   install, update (publishing 1.1) and
#                                        remove with the flatpak CLI, to prove
#                                        the remote works; run build again
#                                        after it to start over at 1.0
#
# dir defaults to /work/flatpak/test. It then holds:
#   repo/                    the remote (OSTree, signed, with AppStream)
#   gpg/, key.gpg            the test signing key (no passphrase)
#   test.flatpakrepo         the remote as a file to open
#   org.test.Hello.flatpakref
#   user/, system/, etc/     empty test installations and flatpak config
#   env                      `. dir/env` points flatpak and libflatpak at them
set -euo pipefail

cmd=${1:?usage: test-remote.sh build|bump|check [dir]}
base=${2:-/work/flatpak/test}
arch=$(flatpak --default-arch)

app=org.test.Hello
addon=org.test.Hello.Plugin.Extra
runtime=org.test.Platform
rtext=org.test.Platform.Ext

gpg_args() {
    echo "--gpg-sign=$(cat "$base/key.id") --gpg-homedir=$base/gpg"
}

env_file() {
    cat >"$base/env" <<EOF
# Source this: flatpak and libflatpak then use only these installations.
export FLATPAK_USER_DIR=$base/user
export FLATPAK_SYSTEM_DIR=$base/system
export FLATPAK_CONFIG_DIR=$base/etc
export FLATPAK_RUN_DIR=$base/run
EOF
}

# An app-info catalog for one component, as flatpak-builder would leave it
# in files/share/app-info: build-update-repo merges these into the remote's
# AppStream branch.
app_info() {
    local dir=$1 id=$2 xml=$3
    mkdir -p "$dir/files/share/app-info/xmls" \
        "$dir/files/share/app-info/icons/flatpak/64x64" \
        "$dir/files/share/app-info/icons/flatpak/128x128"
    printf '<?xml version="1.0" encoding="UTF-8"?>\n<components version="0.8" origin="%s">\n%s\n</components>\n' \
        "$id" "$xml" | gzip -n >"$dir/files/share/app-info/xmls/$id.xml.gz"
    magick -size 64x64 xc:'#6858e2' "$dir/files/share/app-info/icons/flatpak/64x64/$id.png"
    magick -size 128x128 xc:'#6858e2' "$dir/files/share/app-info/icons/flatpak/128x128/$id.png"
}

build_runtime() {
    local dir=$base/build/runtime
    rm -rf "$dir"
    mkdir -p "$dir/files/bin" "$dir/files/lib64"
    cat >"$dir/metadata" <<EOF
[Runtime]
name=$runtime
runtime=$runtime/$arch/stable
sdk=$runtime/$arch/stable

[Extension $rtext]
directory=ext
no-autodownload=true
autodelete=true
EOF
    # bash and the libraries it needs, so `flatpak run` works in a test.
    cp /usr/bin/bash "$dir/files/bin/"
    ldd /usr/bin/bash | awk '/=> \// {print $3} /^\t\/lib64/ {print $1}' |
        while read -r lib; do cp -L "$lib" "$dir/files/lib64/"; done
    ln -s bash "$dir/files/bin/sh"
    app_info "$dir" "$runtime" "  <component type=\"runtime\">
    <id>$runtime</id>
    <name>Test Platform</name>
    <summary>The runtime of the Atlas Store's test apps</summary>
    <project_license>MIT</project_license>
  </component>"
    # shellcheck disable=SC2046
    flatpak build-export --runtime --files=files $(gpg_args) "$base/repo" "$dir" stable >/dev/null
}

# The app at a version: 1.0 asks for a Wayland window; 1.1 also wants the
# network, the home folder and the GPU, which the Updater and the Store hold
# back until the user agrees.
build_app() {
    local version=$1 dir=$base/build/app
    rm -rf "$dir"
    mkdir -p "$dir/files/bin" "$dir/files/share/metainfo" \
        "$dir/export/share/applications"
    local shared="ipc;" sockets="wayland;fallback-x11;" extra=""
    if [ "$version" = 1.1 ]; then
        shared="ipc;network;"
        extra=$'filesystems=home;\ndevices=dri;'
    fi
    cat >"$dir/metadata" <<EOF
[Application]
name=$app
runtime=$runtime/$arch/stable
sdk=$runtime/$arch/stable
command=hello

[Context]
shared=$shared
sockets=$sockets
$extra

[Extension $app.Plugin]
directory=plugins
subdirectories=true
no-autodownload=true
autodelete=true
EOF
    printf '#!/bin/sh\necho "Hello %s"\n' "$version" >"$dir/files/bin/hello"
    chmod +x "$dir/files/bin/hello"
    cat >"$dir/export/share/applications/$app.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Hello
Exec=hello
Icon=$app
EOF
    local component="  <component type=\"desktop-application\">
    <id>$app</id>
    <name>Hello</name>
    <name xml:lang=\"de\">Hallo</name>
    <summary>Says hello, for the Atlas Store's tests</summary>
    <project_license>MIT</project_license>
    <developer id=\"net.eterneon\"><name>Eterneon</name></developer>
    <description>
      <p>A test app with <em>no</em> real use.</p>
      <ul><li>It prints <code>Hello</code></li><li>It has an add-on</li></ul>
    </description>
    <launchable type=\"desktop-id\">$app.desktop</launchable>
    <icon type=\"cached\" width=\"64\" height=\"64\">$app.png</icon>
    <icon type=\"cached\" width=\"128\" height=\"128\">$app.png</icon>
    <categories><category>Utility</category><category>Education</category></categories>
    <keywords><keyword>greeting</keyword><keyword>salutation</keyword></keywords>
    <url type=\"homepage\">https://github.com/EternalCoder454/atlasos-store</url>
    <content_rating type=\"oars-1.1\"/>
    <releases>
      <release version=\"1.1\" date=\"2026-10-02\"><description><p>Now with the network.</p></description></release>
      <release version=\"1.0\" date=\"2026-10-01\"/>
    </releases>
  </component>"
    if [ "$version" = 1.0 ]; then
        component=${component/<release version=\"1.1\" date=\"2026-10-02\"><description><p>Now with the network.<\/p><\/description><\/release>/}
    fi
    printf '<?xml version="1.0" encoding="UTF-8"?>\n%s\n' "$component" | sed 's/^  //' \
        >"$dir/files/share/metainfo/$app.metainfo.xml"
    app_info "$dir" "$app" "$component"
    # No exported icon: flatpak's icon validator needs user namespaces (for
    # bwrap and glycin), which the rootless container doesn't have. The Store
    # takes icons from the AppStream catalog, which has them.
    # shellcheck disable=SC2046
    flatpak build-export $(gpg_args) "$base/repo" "$dir" stable >/dev/null
    # Another branch of the app, which shares the app's data folder.
    # shellcheck disable=SC2046
    flatpak build-export $(gpg_args) "$base/repo" "$dir" beta >/dev/null
}

# An extension of the runtime, which goes when the runtime goes.
build_rtext() {
    local dir=$base/build/rtext
    rm -rf "$dir"
    mkdir -p "$dir/files"
    cat >"$dir/metadata" <<EOF
[Runtime]
name=$rtext

[ExtensionOf]
ref=runtime/$runtime/$arch/stable
EOF
    echo ext >"$dir/files/ext.txt"
    app_info "$dir" "$rtext" "  <component type=\"addon\">
    <id>$rtext</id>
    <extends>$runtime</extends>
    <name>Platform Extension</name>
    <summary>An extension of the test runtime</summary>
    <project_license>MIT</project_license>
  </component>"
    # shellcheck disable=SC2046
    flatpak build-export --runtime --files=files $(gpg_args) "$base/repo" "$dir" stable >/dev/null
}

build_addon() {
    local dir=$base/build/addon
    rm -rf "$dir"
    mkdir -p "$dir/files"
    cat >"$dir/metadata" <<EOF
[Runtime]
name=$addon

[ExtensionOf]
ref=app/$app/$arch/stable
EOF
    echo extra >"$dir/files/extra.txt"
    app_info "$dir" "$addon" "  <component type=\"addon\">
    <id>$addon</id>
    <extends>$app</extends>
    <name>Extra</name>
    <summary>An add-on for Hello</summary>
    <project_license>MIT</project_license>
  </component>"
    # shellcheck disable=SC2046
    flatpak build-export --runtime --files=files $(gpg_args) "$base/repo" "$dir" stable >/dev/null
}

update_repo() {
    # shellcheck disable=SC2046
    flatpak build-update-repo --title="Atlas Store Test" $(gpg_args) "$base/repo" >/dev/null
}

# Every mode works only inside /work/flatpak/ (test installations, never real data).
case $(realpath -m -- "$base") in
/work/flatpak/?*) ;;
*)
    echo "refusing to delete $base: not under /work/flatpak/" >&2
    exit 2
    ;;
esac
case $cmd in
build)
    rm -rf "$base"
    mkdir -p "$base"/{gpg,user,system,etc,run,triggers}
    chmod 700 "$base/gpg"
    gpg --homedir "$base/gpg" --batch --passphrase '' \
        --quick-gen-key 'Atlas Store Test <test@atlas.invalid>' ed25519 sign never 2>/dev/null
    gpg --homedir "$base/gpg" --list-keys --with-colons |
        awk -F: '/^fpr:/ {print $10; exit}' >"$base/key.id"
    gpg --homedir "$base/gpg" --export "$(cat "$base/key.id")" >"$base/key.gpg"
    ostree init --mode=archive-z2 --repo="$base/repo"
    build_runtime
    build_rtext
    build_app 1.0
    build_addon
    update_repo
    key=$(base64 -w0 "$base/key.gpg")
    cat >"$base/test.flatpakrepo" <<EOF
[Flatpak Repo]
Title=Atlas Store Test
Url=file://$base/repo/
GPGKey=$key
EOF
    cat >"$base/$app.flatpakref" <<EOF
[Flatpak Ref]
Title=Hello
Name=$app
Branch=stable
Url=file://$base/repo/
IsRuntime=false
GPGKey=$key
RuntimeRepo=file://$base/test.flatpakrepo
EOF
    env_file
    flatpak repo --branches "$base/repo"
    ;;
bump)
    build_app 1.1
    update_repo
    echo "published $app 1.1"
    ;;
check)
    # shellcheck source=/dev/null
    . "$base/env"
    # The container can't start flatpak's bwrap sandbox (no nested user
    # namespaces), so no install triggers run (empty triggers dir) and the
    # app isn't started: its installed files are checked instead. Running
    # apps is for the AtlasOS test VM.
    mkdir -p "$base/triggers"
    export FLATPAK_TRIGGERSDIR=$base/triggers
    f=flatpak y=(-y --noninteractive)
    $f --user remote-add --gpg-import="$base/key.gpg" test "file://$base/repo/"
    $f --user install "${y[@]}" test "app/$app/$arch/stable" >/dev/null
    grep -q "Hello 1" "$FLATPAK_USER_DIR/app/$app/current/active/files/bin/hello"
    echo "installed $($f --user info "$app" | awk '/Version:|Commit:/ {printf "%s %s ", $1, $2}')"
    $f --user install "${y[@]}" test "$addon" >/dev/null
    $f --user list --columns=application,branch,origin
    $f --user update "${y[@]}" --appstream test >/dev/null
    test -s "$FLATPAK_USER_DIR/appstream/test/$arch/active/appstream.xml.gz"
    echo "appstream: $(zcat "$FLATPAK_USER_DIR/appstream/test/$arch/active/appstream.xml.gz" | grep -c '<component')" components
    # An update that asks for more: publish 1.1, update, and see its new
    # permissions. The remote is left at 1.1; `build` starts it over.
    build_app 1.1
    update_repo
    $f --user update "${y[@]}" "$app" >/dev/null
    grep -q "Hello 1.1" "$FLATPAK_USER_DIR/app/$app/current/active/files/bin/hello"
    grep -q "^filesystems=home;" "$FLATPAK_USER_DIR/app/$app/current/active/metadata"
    echo "updated to 1.1 (adds network, home and dri)"
    # The add-on goes with the app (autodelete). Deleting the data also
    # clears the app's permissions over D-Bus, so a private bus is needed.
    dbus-run-session -- $f --user uninstall "${y[@]}" --delete-data "$app" >/dev/null
    ! $f --user info "$addon" >/dev/null 2>&1
    $f --user uninstall "${y[@]}" --unused >/dev/null
    $f --user remote-delete test
    left=$($f --user list | wc -l)
    [ "$left" = 0 ] || { echo "left installed: $left" >&2; exit 1; }
    echo "check ok"
    ;;
*)
    echo "usage: test-remote.sh build|bump|check [dir]" >&2
    exit 2
    ;;
esac
