pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Controls
import org.kde.kirigami as Kirigami
import Atlas.Ui

// A scrolling grid of AppTiles over an AppListModel (src/catalog.rs). A
// GridView makes cells only for what is on screen, so all of Flathub costs
// what a screenful does. `opened(appId)` is a click or Return on a card.
GridView {
    id: grid

    signal opened(string appId)

    readonly property real minCell: Kirigami.Units.gridUnit * 18
    readonly property int columns: Math.max(1, Math.floor(width / minCell))

    clip: true
    cellWidth: Math.floor(width / columns)
    cellHeight: Math.round(Kirigami.Units.gridUnit * 3.4) + AtlasStyle.spacingLarge * 2 + AtlasStyle.spacing
    boundsBehavior: Flickable.StopAtBounds
    reuseItems: true
    cacheBuffer: cellHeight * 4

    ScrollBar.vertical: AtlasScrollBar {}

    delegate: Item {
        id: cell
        required property int index
        required property string appId
        required property string name
        required property string summary
        required property string developer
        required property string iconSource
        required property bool verified
        required property string sourceTitle

        width: grid.cellWidth
        height: grid.cellHeight

        AppTile {
            anchors.fill: parent
            anchors.rightMargin: AtlasStyle.spacing
            anchors.bottomMargin: AtlasStyle.spacing
            appId: cell.appId
            name: cell.name
            summary: cell.summary
            developer: cell.developer
            iconSource: cell.iconSource
            verified: cell.verified
            sourceTitle: cell.sourceTitle
            onClicked: grid.opened(cell.appId)
        }
    }
}
