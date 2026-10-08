import QtQuick

Rectangle {
    id: root
    property string icon: ""
    property string label: ""
    property bool active: false
    signal clicked
    signal scrolled(int delta)

    implicitWidth: text.implicitWidth + 16
    implicitHeight: 22
    radius: height / 2
    color: active ? "#39bae6" : "#272d38"

    Text {
        id: text
        anchors.centerIn: parent
        text: (root.icon ? root.icon + " " : "") + root.label
        color: root.active ? "#0b0e14" : "#bfbdb6"
        font.pixelSize: 14
    }
    MouseArea {
        anchors.fill: parent
        onClicked: root.clicked()
        onWheel: wheel => root.scrolled(wheel.angleDelta.y > 0 ? 1 : -1)
    }
}
