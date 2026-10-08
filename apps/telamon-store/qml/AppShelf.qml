pragma ComponentBehavior: Bound
import QtQuick
import Telamon.Ui

// A titled row of apps that scrolls sideways (Telamon.Ui's TelamonShelf) with
// the Store's AppTile as the card. `apps` is a list of objects from
// src/featured.rs: { appId, name, summary, developer, iconSource, verified },
// all of it text from the local catalog. The shelf is not shown while the list
// is empty. `appRequested(appId)` is a click or Return on a card.
TelamonShelf {
    id: shelf

    property var apps: []

    signal appRequested(string appId)

    visible: shelf.apps.length > 0
    model: shelf.apps

    delegate: Component {
        AppTile {
            id: tile
            required property var modelData
            width: shelf.cardWidth
            appId: tile.modelData.appId
            name: tile.modelData.name
            summary: tile.modelData.summary
            developer: tile.modelData.developer
            iconSource: tile.modelData.iconSource
            verified: tile.modelData.verified === true
            onClicked: shelf.appRequested(tile.modelData.appId)
        }
    }
}
