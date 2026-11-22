import AppKit
import SwiftUI

/// The sign-in screen. It replaces the whole shell: the brand gradient, the lockup, one glass card with every field
/// and result, and after the first sign-in the one-time Deezer step.
struct ConnectScreen: View {
    let shell: ShellModel
    let model: ConnectModel
    let windowHeight: CGFloat
    @FocusState private var passwordFocused: Bool

    private var theme: Theme { shell.theme }

    var body: some View {
        ScrollView(.vertical) {
            VStack(spacing: T.sectionGap) {
                NaviaLockup(mark: T.connectMark)
                VStack(alignment: .leading, spacing: T.connectGap) {
                    if model.deezerStep { deezerStep } else { form }
                }
                .padding(T.connectCardPad)
                .floatPanel(radius: T.rLG, tinted: true)
                footer.padding(.top, -T.sectionGap / 2)
            }
            .frame(width: T.connectW)
            .padding(.vertical, T.sectionGap)
            .padding(.horizontal, T.gutter)
            .frame(maxWidth: .infinity, minHeight: windowHeight)
        }
        .scrollIndicators(.never)
        .background(LinearGradient(colors: [T.brandTop.color, T.brandBottom.color], startPoint: .top, endPoint: .bottom).ignoresSafeArea())
    }

    private func heading(_ title: String, _ lead: String) -> some View {
        VStack(alignment: .leading, spacing: T.connectHeadingGap) {
            Text(title).font(.navia(T.textSection, .semibold)).tracking(-0.012 * T.textSection).foregroundStyle(T.tEm.color)
            Text(lead).font(.navia(T.textMD)).foregroundStyle(T.t2.color).lineSpacing(T.px(3)).fixedSize(horizontal: false, vertical: true)
        }
        .padding(.bottom, T.connectHeadingGap)
    }

    @ViewBuilder private var form: some View {
        heading(model.title, L("Enter the Navidrome server’s address and the account you listen with."))
        LabelledField(label: L("Server address")) {
            PillTextField(placeholder: "https://music.example.org", text: Bindable(model).address, accent: theme.accent, invalid: model.problem(.address) != nil,
                          onSubmit: submit, autoFocus: model.mode == .firstRun, trailingRoom: T.iconW + T.fieldInset)
                .overlay(alignment: .trailing) {
                    FieldAction(label: L("Test"), enabled: !(model.address.trimmingCharacters(in: .whitespaces).isEmpty || model.isChecking)) {
                        Task { await model.test() }
                    } content: {
                        if model.isTesting { Spinner(color: T.t2).frame(width: T.px(18), height: T.px(18)) } else { Text(L("Test")) }
                    }
                }
        } message: {
            if let problem = model.problem(.address) {
                ConnectStatus(kind: .bad, symbol: "exclamationmark.circle.fill", title: problem.title, detail: problem.detail, theme: theme)
            } else if model.isInsecure {
                ConnectStatus(kind: .warn, symbol: "lock.open.fill", title: L("This connection isn't encrypted"),
                              detail: L("Your password and music travel in the clear over plain HTTP. That's fine at home, but use HTTPS if this server is on the internet."), theme: theme)
            } else if let line = model.serverLine {
                HStack(spacing: T.connectHeadingGap) {
                    Image(systemName: "checkmark.circle.fill").font(.system(size: T.px(16))).foregroundStyle(theme.accentSoft.color)
                    Text(L("Server found") + " · " + line).font(.navia(T.textSM)).foregroundStyle(T.t2.color).fixedSize(horizontal: false, vertical: true)
                }
                .padding(.leading, T.fieldPad)
            }
        }
        LabelledField(label: L("Username")) {
            PillTextField(placeholder: "", text: Bindable(model).user, accent: theme.accent, invalid: model.problem(.account) != nil, onSubmit: submit)
        } message: { EmptyView() }
        LabelledField(label: L("Password")) { passwordField } message: {
            if let problem = model.problem(.account) {
                ConnectStatus(kind: .bad, symbol: "exclamationmark.circle.fill", title: problem.title, detail: problem.detail, theme: theme)
            }
        }
        if let problem = model.problem(.form) {
            ConnectStatus(kind: .bad, symbol: "exclamationmark.circle.fill", title: problem.title, detail: problem.detail, theme: theme)
        }
        HStack(spacing: T.dialogActionsGap) {
            if model.mode == .change {
                ActionButton(title: model.rejected ? L("Browse Offline") : L("Cancel"), fill: true) { shell.cancelConnect() }
            }
            ActionButton(title: model.submitTitle, kind: .prominent(tint: theme.accent), enabled: model.canSubmit || model.isSigningIn, fill: true, busy: model.isSigningIn, action: submit)
        }
        .padding(.top, T.dialogActionsTop)
    }

    private var passwordField: some View {
        Group {
            if model.reveal {
                TextField("", text: Bindable(model).password).textFieldStyle(.plain)
            } else {
                SecureField("", text: Bindable(model).password).textFieldStyle(.plain)
            }
        }
        .font(.navia(T.textMD))
        .focused($passwordFocused)
        .onSubmit(submit)
        .padding(.leading, T.fieldPad)
        .padding(.trailing, T.iconW + T.fieldInset)
        .frame(height: T.controlH)
        .glassPill()
        .focusRing(passwordFocused || model.problem(.account) != nil, model.problem(.account) != nil ? T.invalidRing : theme.accent, width: model.problem(.account) != nil ? T.invalidRingW : T.focusRingWidth)
        .overlay(alignment: .trailing) {
            FieldAction(label: model.reveal ? L("Hide password") : L("Show password"), enabled: true) {
                model.reveal.toggle()
                passwordFocused = true
            } content: {
                Image(systemName: model.reveal ? "eye.slash" : "eye").font(.system(size: T.px(16)))
            }
        }
    }

    @ViewBuilder private var deezerStep: some View {
        heading(L("Use Deezer for recommendations?"), L("Better Autoplay and Artist Radio, with suggestions from Deezer and ListenBrainz added to the server’s, and a Heat column showing each album’s most popular songs."))
        VStack(alignment: .leading, spacing: T.statusGap) {
            fact("paperplane", L("Sends artist and song names to Deezer and ListenBrainz from this computer."))
            fact("key", L("No Deezer account needed. Your login is never sent."))
        }
        .padding(T.statusPadX)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(T.statusFill.color, in: RoundedRectangle(cornerRadius: T.rMD, style: .continuous))
        Text(L("You can change this any time in Settings > Playback.")).font(.navia(T.textSM)).foregroundStyle(T.t3.color).fixedSize(horizontal: false, vertical: true)
        HStack(spacing: T.dialogActionsGap) {
            ActionButton(title: L("Not Now"), fill: true) { shell.answerDeezerStep(false) }
            ActionButton(title: L("Turn On"), kind: .prominent(tint: theme.accent), fill: true) { shell.answerDeezerStep(true) }
        }
        .padding(.top, T.dialogActionsTop)
    }

    private func fact(_ symbol: String, _ text: String) -> some View {
        HStack(alignment: .top, spacing: T.statusGap) {
            Image(systemName: symbol).font(.system(size: T.px(16))).foregroundStyle(T.t2.color).frame(width: T.px(20))
            Text(text).font(.navia(T.textMD)).fixedSize(horizontal: false, vertical: true)
        }
    }

    private var footer: some View {
        HStack(spacing: T.connectHeadingGap) {
            NavidromeLogo(size: T.textLG)
            Text(L("A Navidrome client")).font(.navia(T.textSM))
        }
        .foregroundStyle(T.t3.color)
    }

    private func submit() {
        guard model.canSubmit else { return }
        Task { await model.connect(shell: shell) }
    }
}

/// A labeled field: the label, the field, and its message under it.
struct LabelledField<Field: View, Message: View>: View {
    let label: String
    @ViewBuilder let field: Field
    @ViewBuilder let message: Message

    var body: some View {
        VStack(alignment: .leading, spacing: T.fieldMessageGap) {
            VStack(alignment: .leading, spacing: T.fieldLabelGap) {
                Text(label).font(.navia(T.textSM, .medium)).foregroundStyle(T.t2.color)
                field
            }
            message
        }
    }
}

struct FieldAction<Content: View>: View {
    let label: String
    let enabled: Bool
    let action: () -> Void
    @ViewBuilder let content: Content
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            content
                .font(.navia(T.textSM, .medium))
                .foregroundStyle((hovering && enabled ? T.t1 : T.t2).color)
                .frame(minWidth: T.iconW, minHeight: T.controlH - T.fieldInset * 2)
                .padding(.horizontal, T.fieldPad / 2 - T.fieldInset)
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .opacity(enabled ? 1 : T.disabledOpacity)
        .padding(.trailing, T.fieldInset)
        .onHover { hovering = $0 }
        .help(label)
        .accessibilityLabel(label)
    }
}

struct NavidromeLogo: View {
    let size: CGFloat

    var body: some View {
        if let url = Bundle.main.url(forResource: "navidrome-logo", withExtension: "svg"), let image = NSImage(contentsOf: url) {
            Image(nsImage: image)
                .renderingMode(.template)
                .resizable()
                .interpolation(.high)
                .frame(width: size, height: size)
                .accessibilityLabel("Navidrome")
        }
    }
}

struct ConnectStatus: View {
    enum Kind { case bad, warn }
    let kind: Kind
    let symbol: String?
    let title: String
    let detail: String?
    let theme: Theme

    var body: some View {
        HStack(alignment: .top, spacing: T.statusGap) {
            if let symbol {
                Image(systemName: symbol).font(.system(size: T.px(18))).foregroundStyle(iconColor).frame(width: T.px(20)).padding(.top, T.px(1))
            }
            VStack(alignment: .leading, spacing: T.px(2)) {
                Text(title).font(.navia(T.textMD, .medium))
                if let detail { Text(detail).font(.navia(T.textSM)).foregroundStyle(T.t2.color).fixedSize(horizontal: false, vertical: true) }
            }
            .lineSpacing(T.px(1))
            Spacer(minLength: 0)
        }
        .padding(.vertical, T.statusPadY)
        .padding(.horizontal, T.statusPadX)
        .background(background, in: RoundedRectangle(cornerRadius: T.rMD, style: .continuous))
    }

    private var background: Color { kind == .bad ? T.dangerFill.color : T.statusFill.color }
    private var iconColor: Color { kind == .bad ? T.dangerInk.color : T.t2.color }
}

/// A plain turning ring, so none of the system spinner shows.
struct Spinner: View {
    var color: RGBA = T.t2

    var body: some View {
        TimelineView(T.spinSchedule) { context in
            let turn = context.date.timeIntervalSinceReferenceDate.truncatingRemainder(dividingBy: T.spinPeriod) / T.spinPeriod
            Circle()
                .trim(from: 0, to: 0.72)
                .stroke(color.color, style: StrokeStyle(lineWidth: T.px(2.4), lineCap: .round))
                .rotationEffect(.degrees(turn * 360))
                .padding(T.px(2))
        }
    }
}
