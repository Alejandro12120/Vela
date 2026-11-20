import AppKit

final class FileSource: NSTextField, NSDraggingSource {
    let file: URL

    init(file: URL) {
        self.file = file
        super.init(frame: .zero)
        stringValue = "Arrastra este archivo a Finder"
        alignment = .center
        isEditable = false
        isSelectable = false
        isBordered = false
        drawsBackground = false
    }

    required init?(coder: NSCoder) { nil }

    @objc func copyFile(_ sender: Any?) {
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        window?.title = pasteboard.writeObjects([file as NSURL])
            ? "Archivo copiado"
            : "Error al copiar"
    }

    override func mouseDown(with event: NSEvent) {
        // Keep tracking with this view instead of handing the gesture to a text editor.
    }

    override func mouseDragged(with event: NSEvent) {
        let item = NSDraggingItem(pasteboardWriter: file as NSURL)
        let point = convert(event.locationInWindow, from: nil)
        let frame = NSRect(x: point.x - 16, y: point.y - 16, width: 32, height: 32)
        item.setDraggingFrame(frame, contents: NSWorkspace.shared.icon(forFile: file.path))
        _ = beginDraggingSession(with: [item], event: event, source: self)
    }

    func draggingSession(
        _ session: NSDraggingSession,
        sourceOperationMaskFor context: NSDraggingContext
    ) -> NSDragOperation {
        .copy
    }
}

guard CommandLine.arguments.count == 2 else {
    fputs("Uso: file-handoff ARCHIVO_LOCAL\n", stderr)
    exit(2)
}
let file = URL(fileURLWithPath: CommandLine.arguments[1]).standardizedFileURL
guard (try? file.resourceValues(forKeys: [.isRegularFileKey]).isRegularFile) == true else {
    fputs("Se necesita un archivo local regular.\n", stderr)
    exit(2)
}

let app = NSApplication.shared
app.setActivationPolicy(.regular)
let window = NSWindow(
    contentRect: NSRect(x: 0, y: 0, width: 360, height: 140),
    styleMask: [.titled, .closable],
    backing: .buffered,
    defer: false
)
window.title = "Transferencia de archivos"
window.isReleasedWhenClosed = false
window.center()

let source = FileSource(file: file)
let button = NSButton(title: "Copiar archivo", target: source, action: #selector(FileSource.copyFile(_:)))
let stack = NSStackView(views: [source, button])
stack.orientation = .vertical
stack.alignment = .centerX
stack.distribution = .gravityAreas
stack.spacing = 16
stack.edgeInsets = NSEdgeInsets(top: 24, left: 16, bottom: 24, right: 16)
window.contentView = stack

let menu = NSMenu()
let appItem = NSMenuItem()
let appMenu = NSMenu()
appMenu.addItem(withTitle: "Salir", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
appItem.submenu = appMenu
menu.addItem(appItem)
let editItem = NSMenuItem()
let edit = NSMenu(title: "Edición")
let copy = edit.addItem(withTitle: "Copiar", action: #selector(FileSource.copyFile(_:)), keyEquivalent: "c")
copy.target = source
editItem.submenu = edit
menu.addItem(editItem)
app.mainMenu = menu

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }
}
let delegate = AppDelegate()
app.delegate = delegate
window.makeKeyAndOrderFront(nil)
app.activate(ignoringOtherApps: true)
app.run()
