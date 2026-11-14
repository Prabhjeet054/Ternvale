import AppKit
import Foundation
import Virtualization

/// Hosts `VZVirtualMachineView` in the VMM process. The VM object cannot cross
/// into the SwiftUI app (control-socket-only rule), so this window is the VZ
/// display surface. `custom_display_pipeline` stays false so the app skips Metal.
final class TvVzGuestWindow: NSObject, NSWindowDelegate {
    private var window: NSWindow?
    private weak var session: TvVzSession?

    static func present(session: TvVzSession, width: Int, height: Int) {
        let box = TvVzGuestWindow()
        session.guestWindow = box
        let work = {
            box.build(session: session, width: width, height: height)
        }
        DispatchQueue.main.async(execute: work)
    }

    func close() {
        let work = { [weak self] in
            self?.window?.orderOut(nil)
            self?.window = nil
        }
        if Thread.isMainThread { work() } else { DispatchQueue.main.async(execute: work) }
    }

    private func build(session: TvVzSession, width: Int, height: Int) {
        guard let vm = session.vm else { return }
        // VZVirtualMachineView — macOS 12.0.
        let view = VZVirtualMachineView(frame: NSRect(x: 0, y: 0, width: width / 2, height: height / 2))
        view.virtualMachine = vm
        view.capturesSystemKeys = true
        if #available(macOS 14.0, *) {
            // automaticallyReconfiguresDisplay — macOS 14.0 (WWDC23). macOS guests
            // resize with the window. Pre-14 keeps the create-time pixel size.
            view.automaticallyReconfiguresDisplay = true
            TvLog.emit(.info, "vm=\(session.name) vz view dynamic resolution on (macOS 14+)")
        } else {
            TvLog.emit(
                .info,
                "vm=\(session.name) vz view fixed \(width)x\(height); no automaticallyReconfiguresDisplay"
            )
        }
        let window = NSWindow(
            contentRect: view.frame,
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "Ternvale — \(session.name)"
        window.contentView = view
        window.delegate = self
        window.isReleasedWhenClosed = false
        window.makeKeyAndOrderFront(nil)
        self.window = window
        self.session = session
        NSApp.activate(ignoringOtherApps: true)
        TvLog.emit(.info, "vm=\(session.name) vz VirtualMachineView window shown")
    }

    func windowWillClose(_ notification: Notification) {
        TvLog.emit(.info, "vm=\(session?.name ?? "") vz view window closed by user")
    }
}

/// AppKit run loop on the process main thread (CLI `wait` after `start`).
@_cdecl("tv_vz_nsapp_run")
func tv_vz_nsapp_run() {
    let app = NSApplication.shared
    app.setActivationPolicy(.regular)
    TvLog.emit(.info, "vz NSApplication.run on main thread")
    app.run()
}

@_cdecl("tv_vz_nsapp_stop")
func tv_vz_nsapp_stop() {
    DispatchQueue.main.async {
        NSApp.stop(nil)
        // Unblock `run()` with a dummy event.
        let event = NSEvent.otherEvent(
            with: .applicationDefined,
            location: .zero,
            modifierFlags: [],
            timestamp: 0,
            windowNumber: 0,
            context: nil,
            subtype: 0,
            data1: 0,
            data2: 0
        )
        if let event {
            NSApp.postEvent(event, atStart: true)
        }
        TvLog.emit(.debug, "vz NSApplication.stop posted")
    }
}
