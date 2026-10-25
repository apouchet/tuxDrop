// tuxDrop in the Share menu (Finder, Preview…): lists the devices known to
// tuxdropd and asks it to send the shared files to the one picked; the
// daemon then shows the progress as for the menu bar.
//
// Built by scripts/package-macos.sh into tuxDrop.app/Contents/PlugIns. Like
// every app extension it is sandboxed: share.entitlements lets it reach the
// daemon's control socket, and nothing else.

import AppKit

/// The control socket of tuxdropd (crates/tuxdrop-core/src/ipc.rs): one JSON
/// request and one JSON answer per connection, each on one line.
enum Daemon {
    struct Failure: LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }

    static let notRunning = Failure(message: "tuxDrop is not running. Open tuxDrop from Applications.")

    /// The daemon writes its socket path there: the sandbox gives this
    /// extension another temporary folder than the daemon's.
    static var pathFile: String {
        // NSHomeDirectory() is the sandbox container.
        let home = getpwuid(getuid()).map { String(cString: $0.pointee.pw_dir) } ?? NSHomeDirectory()
        return home + "/Library/Application Support/tuxdrop/control-socket"
    }

    static func call(_ request: [String: Any]) throws -> Any {
        guard
            let path = try? String(contentsOfFile: pathFile, encoding: .utf8)
                .trimmingCharacters(in: .whitespacesAndNewlines),
            !path.isEmpty
        else { throw notRunning }

        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw Failure(message: String(cString: strerror(errno))) }
        defer { close(fd) }
        var timeout = timeval(tv_sec: 5, tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(path.utf8)
        guard bytes.count < MemoryLayout.size(ofValue: addr.sun_path) else { throw notRunning }
        withUnsafeMutableBytes(of: &addr.sun_path) { $0.copyBytes(from: bytes) }
        let connected = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard connected == 0 else { throw notRunning }

        var line = try JSONSerialization.data(withJSONObject: request)
        line.append(UInt8(ascii: "\n"))
        try line.withUnsafeBytes { buf in
            var sent = 0
            while sent < buf.count {
                let n = write(fd, buf.baseAddress! + sent, buf.count - sent)
                guard n > 0 else { throw Failure(message: "tuxDrop is not answering") }
                sent += n
            }
        }

        var answer = Data()
        var chunk = [UInt8](repeating: 0, count: 16384)
        while true {
            let n = read(fd, &chunk, chunk.count)
            if n <= 0 { break }
            answer.append(contentsOf: chunk[..<n])
        }
        guard
            let first = answer.split(separator: UInt8(ascii: "\n")).first,
            let value = try? JSONSerialization.jsonObject(with: Data(first), options: .fragmentsAllowed)
        else { throw Failure(message: "tuxDrop is not answering") }
        if let error = (value as? [String: Any])?["error"] as? String {
            throw Failure(message: error)
        }
        return value
    }

    /// Devices seen in the last 5 minutes, sorted by label.
    static func devices() throws -> [Device] {
        let peers = (try call(["method": "list_peers"]) as? [String: Any])?["peers"] as? [[String: Any]] ?? []
        return peers.compactMap(Device.init).sorted {
            $0.label.localizedCaseInsensitiveCompare($1.label) == .orderedAscending
        }
    }
}

struct Device: Equatable {
    let fingerprint: String
    /// Same as the menu bar: "Name (alias)" for a device named locally.
    let label: String

    init?(_ peer: [String: Any]) {
        guard let fingerprint = peer["fingerprint"] as? String else { return nil }
        let alias = peer["alias"] as? String ?? ""
        let name = peer["name"] as? String ?? ""
        self.fingerprint = fingerprint
        if name.isEmpty || name == alias {
            label = alias
        } else {
            label = alias.isEmpty ? name : "\(name) (\(alias))"
        }
    }
}

@objc(ShareViewController)
final class ShareViewController: NSViewController, NSTableViewDataSource, NSTableViewDelegate {
    private let queue = DispatchQueue(label: "org.tuxdrop.share")
    private var paths: [String] = []
    private var devices: [Device] = []
    private var refresh: Timer?
    private var sending = false

    private let header = NSTextField(labelWithString: "Send to a nearby device")
    private let table = NSTableView()
    private let status = NSTextField(wrappingLabelWithString: "")
    private let spinner = NSProgressIndicator()
    private let sendButton = NSButton(title: "Send", target: nil, action: nil)

    override func loadView() {
        let root = NSView(frame: NSRect(x: 0, y: 0, width: 400, height: 330))

        header.font = .boldSystemFont(ofSize: NSFont.systemFontSize + 1)
        header.lineBreakMode = .byTruncatingMiddle

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("device"))
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.rowHeight = 26
        table.dataSource = self
        table.delegate = self
        table.target = self
        table.doubleAction = #selector(send)
        if #available(macOS 11.0, *) { table.style = .inset }

        let scroll = NSScrollView()
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder

        spinner.style = .spinning
        spinner.controlSize = .small
        spinner.isDisplayedWhenStopped = false
        status.textColor = .secondaryLabelColor
        status.font = .systemFont(ofSize: NSFont.smallSystemFontSize)

        let cancelButton = NSButton(title: "Cancel", target: self, action: #selector(cancel))
        cancelButton.keyEquivalent = "\u{1b}"
        sendButton.target = self
        sendButton.action = #selector(send)
        sendButton.keyEquivalent = "\r"
        sendButton.isEnabled = false

        let statusRow = NSStackView(views: [spinner, status])
        statusRow.spacing = 6
        let buttons = NSStackView(views: [NSView(), cancelButton, sendButton])
        let stack = NSStackView(views: [header, scroll, statusRow, buttons])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 10
        stack.edgeInsets = NSEdgeInsets(top: 16, left: 16, bottom: 16, right: 16)
        stack.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: root.topAnchor),
            stack.bottomAnchor.constraint(equalTo: root.bottomAnchor),
            stack.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            header.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -32),
            scroll.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -32),
            scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 170),
            statusRow.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -32),
            buttons.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -32),
            sendButton.widthAnchor.constraint(greaterThanOrEqualToConstant: 80),
            cancelButton.widthAnchor.constraint(greaterThanOrEqualToConstant: 80),
        ])
        view = root
        preferredContentSize = root.frame.size
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        loadPaths()
        show(status: "Looking for nearby devices…", busy: true)
        queue.async {
            // Nearby devices answer within a couple of seconds.
            _ = try? Daemon.call(["method": "announce"])
        }
        refreshDevices()
        refresh = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
            self?.refreshDevices()
        }
    }

    override func viewWillDisappear() {
        super.viewWillDisappear()
        refresh?.invalidate()
        refresh = nil
    }

    /// The shared files and folders, from the file URLs of the attachments.
    private func loadPaths() {
        let items = extensionContext?.inputItems as? [NSExtensionItem] ?? []
        let providers = items.flatMap { $0.attachments ?? [] }
        let fileURL = "public.file-url"
        let group = DispatchGroup()
        for provider in providers where provider.hasItemConformingToTypeIdentifier(fileURL) {
            group.enter()
            _ = provider.loadObject(ofClass: URL.self) { url, _ in
                DispatchQueue.main.async {
                    if let url, url.isFileURL {
                        self.paths.append(url.path)
                    }
                    group.leave()
                }
            }
        }
        group.notify(queue: .main) { self.updateHeader() }
    }

    private func updateHeader() {
        switch paths.count {
        case 0: header.stringValue = "Only files and folders can be sent"
        case 1: header.stringValue = "Send “\((paths[0] as NSString).lastPathComponent)” to:"
        default: header.stringValue = "Send \(paths.count) items to:"
        }
        updateSendButton()
    }

    private func refreshDevices() {
        queue.async {
            let result = Result { try Daemon.devices() }
            DispatchQueue.main.async { self.show(result) }
        }
    }

    private func show(_ result: Result<[Device], Error>) {
        guard !sending else { return }
        switch result {
        case .success(let found):
            if found != devices {
                let selected = table.selectedRow >= 0 ? devices[table.selectedRow].fingerprint : nil
                devices = found
                table.reloadData()
                if let row = devices.firstIndex(where: { $0.fingerprint == selected }) {
                    table.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
                } else if devices.count == 1 {
                    table.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
                }
            }
            if devices.isEmpty {
                show(status: "Looking for nearby devices… Open LocalSend or tuxDrop on the other device, on the same network.", busy: true)
            } else {
                show(status: "", busy: false)
            }
        case .failure(let error):
            devices = []
            table.reloadData()
            show(status: error.localizedDescription, busy: false)
        }
        updateSendButton()
    }

    private func show(status text: String, busy: Bool) {
        status.stringValue = text
        if busy { spinner.startAnimation(nil) } else { spinner.stopAnimation(nil) }
    }

    private func updateSendButton() {
        sendButton.isEnabled = !sending && !paths.isEmpty && table.selectedRow >= 0
    }

    @objc private func send() {
        guard !sending, !paths.isEmpty, table.selectedRow >= 0 else { return }
        let device = devices[table.selectedRow]
        sending = true
        updateSendButton()
        show(status: "Sending to \(device.label)…", busy: true)
        let request: [String: Any] = ["method": "send", "target": device.fingerprint, "paths": paths]
        queue.async {
            let result = Result { try Daemon.call(request) }
            DispatchQueue.main.async {
                switch result {
                case .success:
                    // The daemon carries on and shows the progress.
                    self.extensionContext?.completeRequest(returningItems: nil)
                case .failure(let error):
                    self.sending = false
                    self.show(status: error.localizedDescription, busy: false)
                    self.updateSendButton()
                }
            }
        }
    }

    @objc private func cancel() {
        extensionContext?.cancelRequest(withError: NSError(domain: NSCocoaErrorDomain, code: NSUserCancelledError))
    }

    func numberOfRows(in tableView: NSTableView) -> Int { devices.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        let cell = NSTableCellView()
        let text = NSTextField(labelWithString: devices[row].label)
        text.lineBreakMode = .byTruncatingTail
        text.translatesAutoresizingMaskIntoConstraints = false
        cell.addSubview(text)
        cell.textField = text
        NSLayoutConstraint.activate([
            text.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 4),
            text.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -4),
            text.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
        ])
        return cell
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        updateSendButton()
    }
}
