import QtQuick
import QtQuick.Layouts
import Telamon.Ui

// The Updates place: app updates (src/updates.rs). Stub.
TelamonPage {
    id: page

    required property var updates
    required property var jobs

    title: qsTr("Updates")

    signal appRequested(string appId)
    signal openSettings

    TelamonEmptyState {
        symbol: Symbols.Update
        title: qsTr("Updates")
        text: qsTr("Not built yet.")
    }
}
