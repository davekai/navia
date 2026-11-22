import Foundation
import Observation

// The equalizer's saved settings and curve math. A port of `apps/navia-gpui/src/eq_model.rs`, so both apps
// keep the same values and JSON. When off, `EqState.plan(for:)` is nil so the output stays bit-exact.

let eqBands: [Double] = [31, 62, 125, 250, 500, 1000, 2000, 4000, 8000, 16000]
let eqBandLabels = ["31 Hz", "62 Hz", "125 Hz", "250 Hz", "500 Hz", "1 kHz", "2 kHz", "4 kHz", "8 kHz", "16 kHz"]
let eqBandShort = ["31", "62", "125", "250", "500", "1k", "2k", "4k", "8k", "16k"]
let eqRange = 12.0
let eqFreqMin = 20.0
let eqFreqMax = 20000.0
let eqMaxFilters = 10
let eqNameMax = 30
private let eqSamples = 180
private let eqGraphicQ = 1.41
private let eqSampleRate = 48000.0

enum EqMode: String { case graphic, parametric }

struct EqFilter: Identifiable, Equatable {
    var id: UInt64
    var freq: Double
    var gain: Double
    var q: Double
}

struct EqProfile: Equatable {
    var mode: EqMode = .graphic
    /// The preset in use for graphic mode, or empty once the curve has been edited.
    var preset = N_("Flat")
    var parametricPreset = ""
    var gains = [Double](repeating: 0, count: 10)
    var filters: [EqFilter] = []
    var preamp = 0.0
    var limiter = true
    /// The headphone correction by name, or empty for none.
    var headphone = ""
    var balance = 0.0
    var mono = false
    var crossfeed = false
    var crossfeedAmount = 35.0
}

struct EqCustomPreset: Equatable {
    var name: String
    var mode: EqMode
    var gains: [Double]?
    var filters: [EqFilter]?
}

enum EqFilterKind: String { case peaking = "pk", lowShelf = "ls", highShelf = "hs" }

struct EqCorrectionFilter: Equatable {
    var kind: EqFilterKind
    var freq: Double
    var gain: Double
    var q: Double
}

enum EqCorrection: Equatable {
    /// Ten graphic bands, only for sample data.
    case bands([Double])
    /// A published AutoEQ profile, with its filters and preamp.
    case filters(preamp: Double, filters: [EqCorrectionFilter])
}

struct EqHeadphone: Equatable {
    var name: String
    var group: String
    var correction: EqCorrection
}

let eqBuiltInPresets: [(name: String, gains: [Double])] = [
    (N_("Flat"), [0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
    (N_("Bass Boost"), [6, 5, 4, 2, 0.5, 0, 0, 0, 0, 0]),
    (N_("Treble Boost"), [0, 0, 0, 0, 0, 0.5, 2, 3.5, 5, 6]),
    (N_("Vocal"), [-2, -2, -1, 1, 3, 4, 3.5, 2, 0, -1]),
    (N_("Loudness"), [6, 4.5, 2, 0, -1, -1.5, -1, 1, 3.5, 5]),
    (N_("Rock"), [4, 3, 1.5, -1, -1.5, -0.5, 1.5, 3, 4, 4]),
    (N_("Electronic"), [5, 4, 1, 0, -1.5, 1, 0.5, 1.5, 4, 4.5]),
    (N_("Acoustic"), [3, 3, 2, 0.5, 1, 1, 2, 2.5, 2, 1.5]),
    (N_("Reduced Bass"), [-5, -4, -3, -1.5, 0, 0, 0, 0, 0, 0]),
    (N_("Spoken Word"), [-4, -3, -1, 1, 3, 3.5, 3, 1.5, -1, -3]),
]

/// Made-up curves for sample data. Real runs load AutoEQ files.
func eqSampleHeadphones() -> [EqHeadphone] {
    func row(_ name: String, _ group: String, _ gains: [Double]) -> EqHeadphone { EqHeadphone(name: name, group: group, correction: .bands(gains)) }
    return [
        row("Sennheiser HD 600", "Over-ear (sample)", [4, 2.5, 0.5, -0.5, 0, 0.5, -1, 1.5, -2, 2]),
        row("Sennheiser HD 650", "Over-ear (sample)", [5, 3, 0, -1, 0, 1, -0.5, 2, -1.5, 3]),
        row("Beyerdynamic DT 770 Pro", "Over-ear (sample)", [1, 0, -1.5, -1, 0.5, 1.5, 2, -1, -4.5, 1]),
        row("Moondrop Aria", "In-ear (sample)", [-1, -1, -0.5, 0, 0.5, 1, 1, -1.5, 1, 4]),
    ]
}

func clampGain(_ v: Double) -> Double { min(eqRange, max(-eqRange, v)) }
func clampFreq(_ f: Double) -> Double { min(eqFreqMax, max(eqFreqMin, f)) }
func freqToX(_ f: Double) -> Double { log(f / eqFreqMin) / log(eqFreqMax / eqFreqMin) }
func xToFreq(_ x: Double) -> Double { eqFreqMin * pow(eqFreqMax / eqFreqMin, x) }
func gainToY(_ g: Double) -> Double { (eqRange - g) / (2 * eqRange) }
func yToGain(_ y: Double) -> Double { eqRange - y * 2 * eqRange }
/// Snaps a dragged frequency to whole Hz below 1 kHz and 10 Hz steps above.
func snapFreq(_ f: Double) -> Double { clampFreq(f < 1000 ? f.rounded() : (f / 10).rounded() * 10) }
func snapGain(_ g: Double) -> Double { clampGain((g * 2).rounded() / 2) }

/// `0`, or a signed value with one decimal and a real minus sign.
func formatDb(_ g: Double) -> String {
    if g == 0 { return "0" }
    return (g > 0 ? "+" : "\u{2212}") + fmtDec(abs(g), 1)
}

func formatFreq(_ f: Double) -> String {
    if f < 1000 { return "\(fmtInt(Int(f.rounded()))) Hz" }
    var digits = f < 10000 ? 2 : 1
    var text = String(format: "%.\(digits)f", f / 1000)
    while digits > 0, text.hasSuffix("0") { text.removeLast(); digits -= 1 }
    return "\(fmtDec(f / 1000, digits)) kHz"
}

func eqBalanceText(_ v: Double) -> String {
    if v == 0 { return L("Center") }
    return v < 0 ? L("{amount}% left", ["amount": fmtInt(Int(abs(v).rounded()))]) : L("{amount}% right", ["amount": fmtInt(Int(abs(v).rounded()))])
}

typealias EqCurvePoint = (x: Double, gain: Double)

private func bandCurve(_ gains: [Double]) -> (Double) -> Double {
    let xs = eqBands.map(freqToX)
    let n = gains.count
    let slopes = (0..<n - 1).map { (gains[$0 + 1] - gains[$0]) / (xs[$0 + 1] - xs[$0]) }
    let tangents: [Double] = (0..<n).map { i in
        guard i > 0, i < n - 1 else { return 0 }
        let (a, b) = (slopes[i - 1], slopes[i])
        guard a * b > 0 else { return 0 }
        return (a < 0 ? -1 : 1) * min(abs(a + b) / 2, 3 * min(abs(a), abs(b)))
    }
    return { x in
        if x <= xs[0] { return gains[0] }
        if x >= xs[n - 1] { return gains[n - 1] }
        let i = (0..<n - 1).first { x >= xs[$0] && x <= xs[$0 + 1] } ?? 0
        let h = xs[i + 1] - xs[i]
        let t = (x - xs[i]) / h
        let (t2, t3) = (t * t, t * t * t)
        return (2 * t3 - 3 * t2 + 1) * gains[i] + (t3 - 2 * t2 + t) * h * tangents[i] + (-2 * t3 + 3 * t2) * gains[i + 1] + (t3 - t2) * h * tangents[i + 1]
    }
}

/// Gain in dB of one filter at a frequency, from the standard biquad formulas.
func eqFilterDb(_ kind: EqFilterKind, freq: Double, gain: Double, q: Double, at f: Double) -> Double {
    let a = pow(10, gain / 40)
    let w0 = 2 * .pi * min(freq, eqSampleRate * 0.45) / eqSampleRate
    let (sw, cw) = (sin(w0), cos(w0))
    let alpha = sw / (2 * max(q, 0.01))
    let (b0, b1, b2, a0, a1, a2): (Double, Double, Double, Double, Double, Double)
    switch kind {
    case .peaking:
        (b0, b1, b2, a0, a1, a2) = (1 + alpha * a, -2 * cw, 1 - alpha * a, 1 + alpha / a, -2 * cw, 1 - alpha / a)
    case .lowShelf:
        let s = 2 * a.squareRoot() * alpha
        (b0, b1, b2, a0, a1, a2) = (a * ((a + 1) - (a - 1) * cw + s), 2 * a * ((a - 1) - (a + 1) * cw), a * ((a + 1) - (a - 1) * cw - s), (a + 1) + (a - 1) * cw + s, -2 * ((a - 1) + (a + 1) * cw), (a + 1) + (a - 1) * cw - s)
    case .highShelf:
        let s = 2 * a.squareRoot() * alpha
        (b0, b1, b2, a0, a1, a2) = (a * ((a + 1) + (a - 1) * cw + s), -2 * a * ((a - 1) + (a + 1) * cw), a * ((a + 1) + (a - 1) * cw - s), (a + 1) - (a - 1) * cw + s, 2 * ((a - 1) - (a + 1) * cw), (a + 1) - (a - 1) * cw - s)
    }
    let w = 2 * .pi * min(f, eqSampleRate * 0.45) / eqSampleRate
    let (c1, s1, c2, s2) = (cos(w), sin(w), cos(2 * w), sin(2 * w))
    func mag(_ x0: Double, _ x1: Double, _ x2: Double) -> Double { hypot(x0 + x1 * c1 + x2 * c2, x1 * s1 + x2 * s2) }
    return 20 * log10(mag(b0, b1, b2) / mag(a0, a1, a2))
}

func eqSampleBands(_ gains: [Double]) -> [EqCurvePoint] {
    let at = bandCurve(gains)
    return (0...eqSamples).map { let x = Double($0) / Double(eqSamples); return (x, at(x)) }
}

func eqSampleFilters(_ filters: [EqFilter]) -> [EqCurvePoint] {
    (0...eqSamples).map { i in
        let x = Double(i) / Double(eqSamples)
        return (x, filters.reduce(0) { $0 + eqFilterDb(.peaking, freq: $1.freq, gain: $1.gain, q: $1.q, at: xToFreq(x)) })
    }
}

/// The curve a correction draws, for the dashed line and the peak estimate.
func eqCorrectionCurve(_ correction: EqCorrection) -> [EqCurvePoint] {
    switch correction {
    case .bands(let g): return eqSampleBands(g)
    case .filters(_, let filters):
        return (0...eqSamples).map { i in
            let x = Double(i) / Double(eqSamples)
            return (x, filters.reduce(0) { $0 + eqFilterDb($1.kind, freq: $1.freq, gain: $1.gain, q: $1.q, at: xToFreq(x)) })
        }
    }
}

func eqProfileCurve(_ p: EqProfile) -> [EqCurvePoint] {
    p.mode == .graphic ? eqSampleBands(p.gains) : eqSampleFilters(p.filters)
}

/// The highest level the curve, correction and preamps reach, in dB.
func eqPeakLevel(_ p: EqProfile, correction: EqCorrection?) -> Double {
    let curve = eqProfileCurve(p)
    let extra = correction.map(eqCorrectionCurve)
    var preamp = p.preamp
    if case .filters(let amount, _) = correction { preamp += amount }
    var peak = -Double.infinity
    for (i, point) in curve.enumerated() {
        peak = max(peak, point.gain + (extra?[i].gain ?? 0))
    }
    return peak + preamp
}

/// A spot for a new filter, in the middle of the widest empty stretch of the frequency axis.
func eqEmptiestFreq(_ filters: [EqFilter]) -> Double {
    let xs = [0.0] + filters.map { freqToX($0.freq) }.sorted() + [1.0]
    var best = 0
    for i in 0..<xs.count - 1 where xs[i + 1] - xs[i] > xs[best + 1] - xs[best] { best = i }
    return snapFreq(xToFreq((xs[best] + xs[best + 1]) / 2))
}

/// Parses an AutoEQ `ParametricEQ.txt` in Equalizer APO's format, a `Preamp: -6.4 dB` line then lines like
/// `Filter 1: ON PK Fc 105 Hz Gain -1.5 dB Q 0.70`.
func parseAutoEq(name: String, text: String) -> Result<EqHeadphone, EqImportError> {
    var preamp = 0.0
    var filters: [EqCorrectionFilter] = []
    for raw in text.split(whereSeparator: \.isNewline) {
        let line = raw.trimmingCharacters(in: .whitespaces)
        if line.hasPrefix("Preamp:") {
            let body = line.dropFirst("Preamp:".count).replacingOccurrences(of: "dB", with: "").trimmingCharacters(in: .whitespaces)
            guard let value = Double(body) else { return .failure(EqImportError(message: L("The preamp line isn't a number."))) }
            preamp = value
            continue
        }
        guard line.hasPrefix("Filter"), let colon = line.firstIndex(of: ":") else { continue }
        let tokens = line[line.index(after: colon)...].split(whereSeparator: \.isWhitespace).map(String.init)
        guard let first = tokens.first, first.caseInsensitiveCompare("ON") == .orderedSame, tokens.count > 1 else { continue }
        let kind: EqFilterKind
        switch tokens[1].uppercased() {
        case "PK", "PEQ": kind = .peaking
        case "LS", "LSC": kind = .lowShelf
        case "HS", "HSC": kind = .highShelf
        default: continue
        }
        func number(_ key: String) -> Double? {
            guard let i = tokens.firstIndex(where: { $0.caseInsensitiveCompare(key) == .orderedSame }), i + 1 < tokens.count else { return nil }
            return Double(tokens[i + 1])
        }
        if let freq = number("Fc"), let gain = number("Gain"), let q = number("Q") {
            filters.append(EqCorrectionFilter(kind: kind, freq: freq, gain: gain, q: q))
        }
    }
    guard !filters.isEmpty else { return .failure(EqImportError(message: L("No filters were found. Pick an AutoEQ ParametricEQ.txt file."))) }
    return .success(EqHeadphone(name: name, group: N_("Loaded AutoEQ profiles"), correction: .filters(preamp: preamp, filters: filters)))
}

struct EqImportError: Error { let message: String }

/// The name to show for an imported file, so `HD 600 ParametricEQ.txt` is `HD 600`.
func eqHeadphoneName(fromPath path: String) -> String {
    let stem = ((path as NSString).lastPathComponent as NSString).deletingPathExtension
    var name = stem
    if name.hasSuffix("ParametricEQ") { name.removeLast("ParametricEQ".count) }
    return name.trimmingCharacters(in: .whitespaces)
}

struct EqPlanFilter: Equatable {
    var kind: EqFilterKind
    var freq: Double
    var gainDb: Double
    var q: Double
}

/// The audio chain in order. The limiter comes last so its ceiling holds.
struct EqPlan: Equatable {
    var preampDb: Double
    var filters: [EqPlanFilter]
    var limiter: Bool
    /// -1 is full left, 1 is full right.
    var balance: Double
    var mono: Bool
    /// 0 to 1 when on.
    var crossfeed: Double?
}

let eqOutputDefault = "default"

func eqFlatProfile(nextID: inout UInt64) -> EqProfile {
    func filter(_ freq: Double) -> EqFilter {
        nextID += 1
        return EqFilter(id: nextID, freq: freq, gain: 0, q: 1)
    }
    return EqProfile(filters: [filter(100), filter(1000), filter(8000)])
}

/// Everything the Equalizer page saves. Each output has its own profile, and presets are shared.
struct EqState {
    var enabled = false
    var output = eqOutputDefault
    var profiles: [String: EqProfile] = [:]
    var custom: [EqCustomPreset] = []
    /// AutoEQ profiles loaded from files.
    var headphones: [EqHeadphone] = []
    var nextID: UInt64 = 0

    var current: EqProfile {
        get {
            if let saved = profiles[output] { return saved }
            var counter: UInt64 = 0
            return eqFlatProfile(nextID: &counter)
        }
        set { profiles[output] = newValue }
    }

    mutating func newFilter(freq: Double) -> EqFilter {
        nextID += 1
        return EqFilter(id: nextID, freq: freq, gain: 0, q: 1)
    }

    /// Gives the output a flat profile with its own filter ids on first use.
    mutating func ensureProfile() {
        if profiles[output] == nil { profiles[output] = eqFlatProfile(nextID: &nextID) }
    }

    func correction(for profile: EqProfile, sampleMode: Bool) -> EqCorrection? {
        guard !profile.headphone.isEmpty else { return nil }
        if sampleMode, let sample = eqSampleHeadphones().first(where: { $0.name == profile.headphone }) { return sample.correction }
        return headphones.first { $0.name == profile.headphone }?.correction
    }

    func plan(for output: String, sampleMode: Bool) -> EqPlan? {
        guard enabled else { return nil }
        var state = self
        state.output = output
        let p = state.current
        var filters: [EqPlanFilter]
        switch p.mode {
        case .graphic: filters = zip(eqBands, p.gains).map { EqPlanFilter(kind: .peaking, freq: $0, gainDb: $1, q: eqGraphicQ) }
        case .parametric: filters = p.filters.map { EqPlanFilter(kind: .peaking, freq: $0.freq, gainDb: $0.gain, q: $0.q) }
        }
        var preamp = p.preamp
        switch correction(for: p, sampleMode: sampleMode) {
        case .bands(let g)?: filters += zip(eqBands, g).map { EqPlanFilter(kind: .peaking, freq: $0, gainDb: $1, q: eqGraphicQ) }
        case .filters(let amount, let list)?:
            preamp += amount
            filters += list.map { EqPlanFilter(kind: $0.kind, freq: $0.freq, gainDb: $0.gain, q: $0.q) }
        case nil: break
        }
        return EqPlan(preampDb: preamp, filters: filters, limiter: p.limiter, balance: p.balance / 100, mono: p.mono, crossfeed: p.crossfeed ? p.crossfeedAmount / 100 : nil)
    }

    // MARK: JSON, the same shape as `equalizer.json` in the GPUI app.

    var json: [String: Any] {
        ["enabled": enabled, "output": output, "next_id": nextID,
         "profiles": profiles.mapValues(Self.profileJSON),
         "custom": custom.map(Self.presetJSON),
         "headphones": headphones.map(Self.headphoneJSON)]
    }

    init() {}

    init(json j: [String: Any]) {
        enabled = j["enabled"] as? Bool ?? false
        output = j["output"] as? String ?? eqOutputDefault
        nextID = (j["next_id"] as? NSNumber)?.uint64Value ?? 0
        if let list = j["profiles"] as? [String: Any] {
            for (key, value) in list {
                if let dict = value as? [String: Any], let p = Self.profile(from: dict) { profiles[key] = p }
            }
        }
        custom = (j["custom"] as? [[String: Any]] ?? []).compactMap(Self.preset)
        headphones = (j["headphones"] as? [[String: Any]] ?? []).compactMap(Self.headphone)
        let highest = profiles.values.flatMap(\.filters).map(\.id).max() ?? 0
        nextID = max(nextID, highest)
    }

    private static func filterJSON(_ f: EqFilter) -> [String: Any] { ["id": f.id, "freq": f.freq, "gain": f.gain, "q": f.q] }

    private static func filter(from d: [String: Any]) -> EqFilter? {
        guard let id = (d["id"] as? NSNumber)?.uint64Value, let freq = d["freq"] as? Double, let gain = d["gain"] as? Double, let q = d["q"] as? Double else { return nil }
        return EqFilter(id: id, freq: clampFreq(freq), gain: clampGain(gain), q: min(8, max(0.2, q)))
    }

    private static func gains(from v: Any?) -> [Double]? {
        guard let list = v as? [Double], list.count == 10 else { return nil }
        return list.map(clampGain)
    }

    private static func profileJSON(_ p: EqProfile) -> [String: Any] {
        ["mode": p.mode.rawValue, "preset": p.preset, "parametricPreset": p.parametricPreset, "gains": p.gains, "filters": p.filters.map(filterJSON),
         "preamp": p.preamp, "limiter": p.limiter, "headphone": p.headphone, "balance": p.balance, "mono": p.mono, "crossfeed": p.crossfeed, "crossfeedAmount": p.crossfeedAmount]
    }

    private static func profile(from d: [String: Any]) -> EqProfile? {
        guard let g = gains(from: d["gains"]), let list = d["filters"] as? [[String: Any]] else { return nil }
        var p = EqProfile()
        p.mode = EqMode(rawValue: d["mode"] as? String ?? "") ?? .graphic
        p.preset = d["preset"] as? String ?? ""
        p.parametricPreset = d["parametricPreset"] as? String ?? ""
        p.gains = g
        p.filters = list.compactMap(filter)
        p.preamp = clampGain(d["preamp"] as? Double ?? 0)
        p.limiter = d["limiter"] as? Bool ?? true
        p.headphone = d["headphone"] as? String ?? ""
        p.balance = min(100, max(-100, d["balance"] as? Double ?? 0))
        p.mono = d["mono"] as? Bool ?? false
        p.crossfeed = d["crossfeed"] as? Bool ?? false
        p.crossfeedAmount = min(100, max(0, d["crossfeedAmount"] as? Double ?? 35))
        return p
    }

    private static func presetJSON(_ p: EqCustomPreset) -> [String: Any] {
        var out: [String: Any] = ["name": p.name, "mode": p.mode.rawValue]
        if let gains = p.gains { out["gains"] = gains }
        if let filters = p.filters { out["filters"] = filters.map(filterJSON) }
        return out
    }

    private static func preset(from d: [String: Any]) -> EqCustomPreset? {
        guard let name = d["name"] as? String else { return nil }
        return EqCustomPreset(name: name, mode: EqMode(rawValue: d["mode"] as? String ?? "") ?? .graphic, gains: gains(from: d["gains"]), filters: (d["filters"] as? [[String: Any]])?.compactMap(filter))
    }

    private static func headphoneJSON(_ h: EqHeadphone) -> [String: Any] {
        var preamp = 0.0
        var filters: [[String: Any]] = []
        if case .filters(let amount, let list) = h.correction {
            preamp = amount
            filters = list.map { ["kind": $0.kind.rawValue, "freq": $0.freq, "gain": $0.gain, "q": $0.q] }
        }
        return ["name": h.name, "group": h.group, "preamp": preamp, "filters": filters]
    }

    private static func headphone(from d: [String: Any]) -> EqHeadphone? {
        guard let name = d["name"] as? String, let list = d["filters"] as? [[String: Any]] else { return nil }
        let filters = list.compactMap { f -> EqCorrectionFilter? in
            guard let kind = EqFilterKind(rawValue: f["kind"] as? String ?? "pk") else { return nil }
            return EqCorrectionFilter(kind: kind, freq: f["freq"] as? Double ?? 1000, gain: f["gain"] as? Double ?? 0, q: f["q"] as? Double ?? 1)
        }
        return EqHeadphone(name: name, group: d["group"] as? String ?? N_("Loaded AutoEQ profiles"), correction: .filters(preamp: d["preamp"] as? Double ?? 0, filters: filters))
    }
}

/// The page's model, saved as the GPUI app's JSON under the `equalizer` default.
@Observable final class EqualizerStore {
    private(set) var state: EqState

    init() {
        let defaults = UserDefaults.standard
        if let text = defaults.string(forKey: "equalizer"), let data = text.data(using: .utf8),
           let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] {
            state = EqState(json: object)
        } else {
            state = EqState()
        }
    }

    func update(_ change: (inout EqState) -> Void) {
        var next = state
        change(&next)
        state = next
    }

    /// Saves the settings. Sliders call this when a drag ends, not on every move.
    func save() {
        if JSONSerialization.isValidJSONObject(state.json), let data = try? JSONSerialization.data(withJSONObject: state.json) {
            UserDefaults.standard.set(String(decoding: data, as: UTF8.self), forKey: "equalizer")
        }
    }

    func updateAndSave(_ change: (inout EqState) -> Void) {
        update(change)
        save()
    }

    func summary(outputLabel: String) -> String {
        guard state.enabled else { return L("Off. Audio plays unprocessed.") }
        let p = state.current
        let name = (p.mode == .graphic ? p.preset : p.parametricPreset)
        return name.isEmpty ? L("On, using a custom curve for {output}.", ["output": outputLabel]) : L("On, using {name} for {output}.", ["name": L(name), "output": outputLabel])
    }
}
