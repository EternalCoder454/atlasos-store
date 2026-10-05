import QtQuick
import Atlas.Ui

// A page that isn't built yet, or a message in place of one: a symbol, a
// heading and a line of plain text (it may hold what a launch asked for).
AtlasPage {
    id: page

    // A message about a launch, not a place: the next one takes its spot.
    property bool message: false
    property int symbol: Symbols.Construction
    property string heading
    property string text

    AtlasEmptyState {
        symbol: page.symbol
        title: page.heading
        text: page.text
    }
}
