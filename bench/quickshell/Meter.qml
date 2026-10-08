import QtQuick
import QtQuick.Layouts

RowLayout {
    property string icon: ""
    property string label: ""
    property real value: 0
    spacing: 4

    Text { text: parent.icon; color: "#e6b450"; font.pixelSize: 14 }
    Text { text: parent.label; color: "#bfbdb6"; font.pixelSize: 14 }
    Rectangle {
        implicitWidth: 48; implicitHeight: 6; radius: 3; color: "#272d38"
        Rectangle {
            height: parent.height; radius: 3
            width: 48 * Math.max(0, Math.min(1, value))
            color: value > 0.85 ? "#d95757" : value > 0.6 ? "#ffb454" : "#39bae6"
        }
    }
}
