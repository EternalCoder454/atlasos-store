pragma ComponentBehavior: Bound
import QtQuick

// The native apps' JSON (src/native.rs, `appsJson`) as the objects AppShelf
// and AppTile take: { appId, name, summary, developer, iconSource, verified,
// letter }. Texts are cleaned manifest fields, shown as plain text.
QtObject {
    id: root

    property string json: "[]"

    function parse(text) {
        try {
            const list = JSON.parse(text);
            return Array.isArray(list) ? list : [];
        } catch (e) {
            return [];
        }
    }

    readonly property var all: root.parse(root.json)

    // The apps as tiles; an app that cannot run here is left out of the shelf
    // unless it is installed.
    readonly property var tiles: root.all.filter(a => a.state !== "incompatible").map(a => ({
                appId: a.id,
                name: a.name,
                summary: a.summary,
                developer: a.installed ? (a.state === "update" ? qsTr("Update available") : qsTr("Installed")) : qsTr("Telamon app"),
                iconSource: a.iconSource,
                verified: false,
                letter: true
            }))
}
