import AppKit
import NaviaCoreFFI
import Observation

/// AirPlay receivers as outputs. The core's `airplay` crate does the protocol. Picking a receiver moves playback
/// there, and losing it pauses the player. The core never falls back to this Mac's speakers on its own.
@Observable final class AirPlayModel {
    /// One per app, like the core's service. Each account's player opens receivers through it.
    static let shared = AirPlayModel()
    struct Receiver: Equatable, Identifiable {
        enum Support: String { case ready, restricted, needsPtp, airplay1, password }
        enum State: String { case connecting, playing, needsPairing, firewall, failed }
        let id: String
        let name: String
        /// Manufacturer and model. Empty while the receiver isn't on the network.
        let model: String
        let support: Support
        let paired: Bool
        let needsPin: Bool
        /// Seen on the network now. A paired receiver that is asleep is listed with false.
        let found: Bool
        let state: State?

        var canPick: Bool { support == .ready && found }
    }

    enum PinPhase: Equatable { case waiting, checking, wrong, expired }
    struct PinRequest: Equatable { let id: String; let name: String; var phase: PinPhase }

    private(set) var receivers: [Receiver] = []
    /// Discovery is running, for the first seconds after the menu opens or the service starts.
    private(set) var searching = false
    private(set) var pin: PinRequest?
    /// from the shell's output choice.
    private(set) var activeID: String?
    /// The active receiver went away after playing. The player paused, and the chip explains.
    private(set) var lost = false

    @ObservationIgnored private(set) var service: NaviaAirPlay?
    @ObservationIgnored private var sampleState: String?
    @ObservationIgnored private var searchTask: Task<Void, Never>?
    @ObservationIgnored private var wakeObserver: NSObjectProtocol?
    @ObservationIgnored private var lastActiveState: Receiver.State?
    @ObservationIgnored private var hasPlayed = false
    @ObservationIgnored var onLost: (() -> Void)?
    @ObservationIgnored var setOutput: ((String?) -> Void)?
    /// The receiver's volume changed, 0 to 1, or a remote button was pressed.
    @ObservationIgnored var onRemote: ((AirPlayRemote) -> Void)?

    static let idPrefix = "airplay:"
    private static let searchSeconds: Double = 2.5

    // MARK: Service

    /// Starts discovery for this core. Never in a test or capture run, which mustn't reach a device.
    func startService(core: NaviaCore) -> NaviaAirPlay? {
        let model = self
        guard !CoreMode.isTestRun, !UserDefaults.standard.bool(forKey: "naviaCapture") else { return nil }
        let dir = URL.applicationSupportDirectory.appending(path: "Navia")
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        do {
            let service = try core.startAirplay(configDir: dir.path, secrets: AirPlayKeychain(), listener: AirPlayEvents(model))
            model.attach(service)
            return service
        } catch {
            Log.app.error("AirPlay is off: \(String(describing: error), privacy: .public)")
            return nil
        }
    }

    private func attach(_ service: NaviaAirPlay) {
        self.service = service
        beginSearch()
        reload()
        // receivers that went quiet while the Mac slept are only found again by asking.
        wakeObserver = NSWorkspace.shared.notificationCenter.addObserver(forName: NSWorkspace.didWakeNotification, object: nil, queue: .main) { _ in
            service.rescan()
        }
    }

    func reload() {
        guard sampleState == nil, let service else { return }
        let rows = service.outputs()
        receivers = rows.map { row in
            Receiver(id: row.id, name: row.name, model: row.model, support: row.unsupported.flatMap(Receiver.Support.init(rawValue:)) ?? .ready,
                     paired: row.paired, needsPin: row.needsPin, found: row.found, state: row.state.flatMap(Receiver.State.init(rawValue:)))
        }
        followActive()
    }

    /// Shows the discovery spinner briefly when the menu opens, since it may still be finding receivers.
    func menuOpened() {
        guard sampleState == nil else { return }
        service?.rescan()
        beginSearch()
    }

    private func beginSearch() {
        searching = true
        searchTask?.cancel()
        searchTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(Self.searchSeconds))
            guard !Task.isCancelled else { return }
            self?.searching = false
        }
    }

    // MARK: The output

    func outputChanged(_ id: String?) {
        let next = id.flatMap { $0.hasPrefix(Self.idPrefix) ? $0 : nil }
        guard next != activeID else { return }
        activeID = next
        lost = false
        lastActiveState = nil
        hasPlayed = false
        followActive()
    }

    var active: Receiver? { activeID.flatMap { id in receivers.first { $0.id == id } } }

    var activeState: Receiver.State? { active?.state }

    /// When playing fails, pauses once and shows the chip, which stays through the reconnect. A failed first
    /// connection shows the chip too.
    private func followActive() {
        guard let state = activeState, state != lastActiveState else { return }
        switch state {
        case .playing:
            lost = false
            hasPlayed = true
        case .failed where !lost:
            lost = true
            if hasPlayed { onLost?() }
        default: break
        }
        lastActiveState = state
    }

    /// The player has given up reconnecting.
    var lostGaveUp: Bool { lost && activeState != .connecting }

    /// Asks for the code if the receiver was never paired, else connects.
    func pick(_ receiver: Receiver) {
        guard receiver.canPick else { return }
        if receiver.needsPin, !receiver.paired {
            pin = PinRequest(id: receiver.id, name: receiver.name, phase: .waiting)
            askForCode(receiver.id)
            return
        }
        setOutput?(receiver.id)
    }

    func retry() {
        guard let id = activeID else { return }
        lost = false
        lastActiveState = nil
        setOutput?(id)
    }

    func playHere() {
        lost = false
        setOutput?(nil)
    }

    // MARK: Pairing

    /// Asks the receiver to show a new code. It blocks in the core for a few seconds, so it runs off the main thread.
    private func askForCode(_ id: String) {
        guard sampleState == nil, let service else { return }
        Task.detached(priority: .userInitiated) { [weak self] in
            let failed: Bool
            do { try service.pairBegin(id: id); failed = false } catch { failed = true }
            await MainActor.run {
                guard let self, self.pin?.id == id, failed else { return }
                self.pin?.phase = .expired
            }
        }
    }

    /// Pairs with the code. A wrong code makes the receiver show a new one, and any other failure counts as a timeout.
    func submit(code: String) {
        guard var request = pin, request.phase != .checking else { return }
        request.phase = .checking
        pin = request
        let id = request.id
        if sampleState != nil {
            Task { [weak self] in
                try? await Task.sleep(for: .seconds(0.9))
                guard let self else { return }
                if code == "0000" { self.pin?.phase = .wrong } else { self.pin = nil }
            }
            return
        }
        guard let service else { return }
        Task.detached(priority: .userInitiated) { [weak self] in
            var outcome: PinPhase? = nil
            do {
                try service.pairFinish(id: id, pin: code)
            } catch {
                outcome = "\(error)".contains("wrong PIN") ? .wrong : .expired
            }
            await MainActor.run {
                guard let self, self.pin?.id == id else { return }
                if let outcome {
                    self.pin?.phase = outcome
                    if outcome == .wrong { self.askForCode(id) }
                } else {
                    self.pin = nil
                    self.reload()
                    self.setOutput?(id)
                }
            }
        }
    }

    /// The next digit after a wrong code starts a new entry.
    func startNewCode() {
        guard pin?.phase == .wrong else { return }
        pin?.phase = .waiting
    }

    /// Asks for a new code after the old one timed out.
    func newCode() {
        guard let id = pin?.id else { return }
        pin?.phase = .waiting
        askForCode(id)
    }

    func cancelPin() { pin = nil }

    // MARK: Remote

    fileprivate func remote(_ command: AirPlayRemote) { onRemote?(command) }

    // MARK: Sample (DEBUG previews)

    #if DEBUG
    /// `-naviaAirPlaySample <state>` shows a state with sample receivers and no network. States: menu, searching,
    /// empty, connecting, pin, pin-wrong, pin-expired, playing, lost, lost-gaveup.
    func applySample(_ state: String) {
        sampleState = state
        let lg = Receiver(id: "airplay:lg", name: "[LG] webOS TV", model: "LG webOS TV", support: .ready, paired: !state.hasPrefix("pin") && state != "menu", needsPin: true, found: true, state: nil)
        var list: [Receiver] = [
            lg,
            Receiver(id: "airplay:mba", name: "MacBook Air (sample)", model: "Mac16,12", support: .restricted, paired: false, needsPin: false, found: true, state: nil),
            Receiver(id: "airplay:s-living", name: "Living Room (sample)", model: "Sample TV", support: .ready, paired: false, needsPin: false, found: true, state: state == "connecting" ? .connecting : nil),
            Receiver(id: "airplay:s-bedroom", name: "Bedroom TV (sample)", model: "", support: .ready, paired: true, needsPin: true, found: false, state: nil),
            Receiver(id: "airplay:s-kitchen", name: "Kitchen (sample)", model: "Sample speaker", support: .needsPtp, paired: false, needsPin: false, found: true, state: nil),
            Receiver(id: "airplay:s-office", name: "Office (sample)", model: "Sample receiver", support: .airplay1, paired: false, needsPin: false, found: true, state: nil),
            Receiver(id: "airplay:s-den", name: "Den (sample)", model: "Sample receiver", support: .password, paired: false, needsPin: false, found: true, state: nil),
        ]
        searching = state == "searching"
        if state == "searching" || state == "empty" { list = [] }
        switch state {
        case "playing", "playing-menu":
            list[0] = Receiver(id: lg.id, name: lg.name, model: lg.model, support: .ready, paired: true, needsPin: true, found: true, state: .playing)
            activeID = lg.id
        case "lost", "lost-gaveup":
            list[0] = Receiver(id: lg.id, name: lg.name, model: lg.model, support: .ready, paired: true, needsPin: true, found: true, state: state == "lost" ? .connecting : .failed)
            activeID = lg.id
            lost = true
        case "connecting":
            activeID = "airplay:s-living"
        case "pin", "pin-wrong", "pin-expired":
            pin = PinRequest(id: lg.id, name: lg.name, phase: state == "pin" ? .waiting : state == "pin-wrong" ? .wrong : .expired)
        default: break
        }
        receivers = list
    }
    var isSample: Bool { sampleState != nil }
    #endif
}

/// Receives core events on a background thread and forwards them to the main thread.
private nonisolated final class AirPlayEvents: AirPlayListener, @unchecked Sendable {
    weak var model: AirPlayModel?
    init(_ model: AirPlayModel) { self.model = model }
    func onRemote(command: AirPlayRemote) { Task { @MainActor [weak self] in self?.model?.remote(command) } }
    func onChanged() { Task { @MainActor [weak self] in self?.model?.reload() } }
}

/// The AirPlay identity, one keychain secret per key. Never logs the values.
private nonisolated final class AirPlayKeychain: SecretStoreCallback, @unchecked Sendable {
    private func account(_ key: String) -> String { "airplay:" + key }
    /// The keychain location older builds used.
    private func legacy(_ key: String) -> NaviaKeychain.Legacy { .init(service: NaviaKeychain.service + ".airplay", account: key) }

    func get(key: String) -> String? {
        NaviaKeychain.read(account(key), legacy: legacy(key)).flatMap { String(data: $0, encoding: .utf8) }
    }

    func set(key: String, value: String) -> Bool {
        NaviaKeychain.write(account(key), Data(value.utf8))
    }

    func delete(key: String) -> Bool {
        NaviaKeychain.delete(account(key), legacy: legacy(key))
    }
}
