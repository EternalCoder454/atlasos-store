import QtQuick
import QtQuick.Layouts
import Telamon.Ui

// The Sources place: the Flatpak remotes (src/sources.rs). Stub.
TelamonPage {
    id: page

    required property var sources
    required property var jobs

    title: qsTr("Sources")

    TelamonEmptyState {
        symbol: Symbols.Dns
        title: qsTr("Sources")
        text: qsTr("Not built yet.")
    }
}
