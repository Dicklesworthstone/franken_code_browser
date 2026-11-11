import SwiftUI
import AppKit

/// A separate live-range surface. Opening it never replaces an exact retained
/// reader or imports an old search, bookmark, outline or saved-archive anchor.
@MainActor struct AtlasObservedReaderView: View {
    let presentation: AtlasObservedReaderPresentation
    private var origin: AtlasObservedReaderSource { presentation.source }
    @State private var model: AtlasObservedReaderModel
    @State private var wrapsLines = false
    @State private var showsLineNumbers = true
    @State private var showsWhitespace = false
    @State private var copyNotice = ""
    @FocusState private var searchFocused: Bool
    @Environment(\.dismiss) private var dismiss

    init(presentation: AtlasObservedReaderPresentation) {
        self.presentation = presentation
        _model = State(initialValue: presentation.model)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Label("Observed Range Reader", systemImage: "doc.text.viewfinder").font(.headline)
                Spacer()
                Button("Reopen current file", action: model.reopenCurrentFile).disabled(!model.canReopen)
                    .help("Open the current pathname as a new descriptor and observe its first range. Keep the accepted observation if opening fails.")
                Button("Done") { presentation.close(); dismiss() }.keyboardShortcut(.cancelAction)
            }
            Text(verbatim: model.info?.identity.displayPath ?? String(reflecting: origin.input.displayPath))
                .font(.system(.caption, design: .monospaced))
                .textSelection(.enabled).lineLimit(2)
            Text(verbatim: "Project: " + String(reflecting: origin.input.root)).font(.caption).foregroundStyle(.secondary)
                .textSelection(.enabled).lineLimit(1).truncationMode(.middle)
            Text("Observe part of a current file, including files larger than the complete reader's 32 MiB limit. Each read is independent; the original complete reader stays unchanged.")
                .font(.caption).foregroundStyle(.secondary)
            if let match = origin.currentMatch {
                Text("Current context for retained match at original bytes \(match.range.start)..<\(match.range.end). This explicitly reopens the current pathname; the earlier search retained only literal bytes.")
                    .font(.caption).foregroundStyle(.secondary)
                if let verification = model.externalContext {
                    Text(verification.message).font(.caption).foregroundStyle(.secondary)
                }
            }
            controls
            searchControls
            Divider()
            if let page = model.page {
                let observation = page.observation
                if let notice = model.verifiedPageNotice {
                    Text(notice).font(.caption).foregroundStyle(.secondary)
                }
                HStack(alignment: .firstTextBaseline) {
                    VStack(alignment: .leading, spacing: 3) {
                        Text("Observation \(observation.id) · original bytes \(page.visibleRange.start)..<\(page.visibleRange.end)")
                            .font(.system(.caption, design: .monospaced))
                        Text("File length observed before read: \(observation.observedLength) bytes · \(observation.request.encoding.label)")
                            .font(.caption).foregroundStyle(.secondary)
                        if let lines = page.lineNavigation {
                            Text("Physical lines \(lines.firstLine)–\(lines.lastLine) · scanned \(lines.scannedInputBytes) bytes · retained \(observation.capturedRange.count) bytes including decoding context")
                                .font(.caption).foregroundStyle(.secondary)
                            if lines.lastLine - lines.firstLine + 1 < lines.requestedLines {
                                Text("The scan reached the end of the file before the full requested line count.")
                                    .font(.caption).foregroundStyle(.secondary)
                            }
                        }
                        if let finalLength = observation.finalObservedLength, finalLength != observation.observedLength {
                            Text("Length after read: \(finalLength) bytes; the file changed during this observation.")
                                .font(.caption).foregroundStyle(.orange)
                        }
                    }
                    Spacer()
                    Button("Copy original hex", action: copyOriginalHex).disabled(!model.canCopy)
                        .help("Copy the exact original bytes of this retained observation, not a new file read or the decoded selection")
                }
                if let source = model.source {
                    AtlasSourceReader(source: source, navigation: model.navigation,
                        selection: model.searchSelection.map { AtlasReaderSelection(source: source, range: $0) },
                        wrapsLines: wrapsLines, showsLineNumbers: showsLineNumbers,
                        showsWhitespace: showsWhitespace, firstPhysicalLine: page.firstPhysicalLine,
                        backgroundColor: Monokai.background, foregroundColor: Monokai.color("plain")) {
                            AtlasDocument.style(text: source.text, runs: [])
                        }
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            } else {
                ContentUnavailableView(model.busy ? "Observing the opened file…" : "No accepted observation",
                    systemImage: "doc.text.viewfinder",
                    description: Text("Opening reads metadata first. Only the requested range and bounded decoding context are loaded."))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            Divider()
            VStack(alignment: .leading, spacing: 4) {
                Text(model.notice).textSelection(.enabled)
                if !copyNotice.isEmpty { Text(copyNotice) }
                Text(model.coverage)
                Text("Selected text copies the displayed decoding. Original hex copies exact bytes. At most eight observations are retained; an evicted range is unavailable until you explicitly observe it again.")
            }
            .font(.caption).foregroundStyle(.secondary)
        }
        .padding(14).frame(minWidth: 880, idealWidth: 1120, minHeight: 740, idealHeight: 900)
        .onAppear { presentation.start() }
        .onDisappear { presentation.close() }
        .onChange(of: model.navigation) { _, _ in copyNotice = "" }
        .onChange(of: model.searchText) { _, _ in copyNotice = "" }
        .onChange(of: model.searchCaseSensitive) { _, _ in copyNotice = "" }
        .onChange(of: model.countAllSearchMatches) { _, _ in copyNotice = "" }
        .onChange(of: model.encoding) { _, _ in copyNotice = "" }
        .background {
            Button("Find in observed file") { searchFocused = true }
                .keyboardShortcut("f", modifiers: .command).hidden().accessibilityHidden(true)
        }
    }

    private var controls: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 7) {
                Button("Back", action: model.back).disabled(!model.canBack)
                    .help("Restore an earlier retained observation without rereading the file")
                Button("Forward", action: model.forward).disabled(!model.canForward)
                    .help("Restore a later retained observation without rereading the file")
                Divider().frame(height: 20)
                Button("Previous bytes", action: model.previousRange).disabled(!model.canPrevious)
                    .help("Read a new observation before this byte range")
                Button("Next bytes", action: model.nextRange).disabled(!model.canNext)
                    .help("Read a new observation after this byte range")
                Button("Observe again", action: model.observeAgain).disabled(model.page == nil || model.busy)
                    .help("Repeat this page's byte or physical-line request against the same opened file object")
                Spacer()
                Menu("Display") {
                    Toggle("Line numbers", isOn: $showsLineNumbers)
                    Toggle("Visible whitespace", isOn: $showsWhitespace)
                    Toggle("Wrap lines", isOn: $wrapsLines)
                }
                .menuStyle(.borderlessButton).fixedSize()
            }
            HStack(spacing: 7) {
                Text("Byte offset").font(.caption)
                TextField("0", text: $model.offsetInput).frame(width: 150)
                    .textFieldStyle(.roundedBorder).onSubmit(model.observeInput)
                    .accessibilityLabel("Absolute original-byte offset")
                Picker("Decode", selection: $model.encoding) {
                    ForEach(AtlasObservedReaderEncoding.allCases) { Text($0.label).tag($0) }
                }.frame(width: 165)
                Picker("Range", selection: $model.pageBytes) {
                    Text("16 KiB").tag(UInt64(16 * 1024))
                    Text("64 KiB").tag(UInt64(64 * 1024))
                    Text("256 KiB").tag(UInt64(256 * 1024))
                }.frame(width: 150)
                Button("Observe", action: model.observeInput).disabled(!model.canObserve)
                    .help("Read a new bounded observation. A newer request cancels an obsolete pending request.")
                Spacer()
                if model.busy {
                    ProgressView().controlSize(.small)
                    Button("Cancel", action: model.cancel)
                }
            }
            HStack(spacing: 7) {
                Text("Line").font(.caption)
                TextField("1", text: $model.lineInput).frame(width: 130)
                    .textFieldStyle(.roundedBorder).onSubmit(model.observeLineInput)
                    .accessibilityLabel("First physical line, starting at one")
                Text("Lines").font(.caption)
                TextField("20", text: $model.lineCountInput).frame(width: 60)
                    .textFieldStyle(.roundedBorder).onSubmit(model.observeLineInput)
                    .accessibilityLabel("Maximum complete lines to read, from one to 4096")
                Picker("Scan up to", selection: $model.lineScanBytes) {
                    Text("16 MiB").tag(UInt64(16 * 1024 * 1024))
                    Text("256 MiB").tag(AtlasObservedReaderLimits.lineScanBytes)
                    Text("1 GiB").tag(UInt64(1024 * 1024 * 1024))
                    Text("4 GiB").tag(AtlasObservedReaderLimits.maximumLineScanBytes)
                }.frame(width: 185)
                Button("Go to lines", action: model.observeLineInput).disabled(!model.canReadLines)
                    .help("Scan from the beginning of the opened file and retain the requested complete lines from that same scan. Range limits the selected bytes.")
                Spacer()
            }
            Text("Line navigation uses the selected decoding and Range allowance. A scan limit preserves the accepted page; it does not mean the line is missing.")
                .font(.caption2).foregroundStyle(.secondary)
            Text("Decoding is explicit for the next observation. It does not determine the encoding of unknown file regions.")
                .font(.caption2).foregroundStyle(.secondary)
        }
    }

    private var searchControls: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 7) {
                TextField("Find text in the opened file", text: $model.searchText)
                    .textFieldStyle(.roundedBorder).focused($searchFocused).onSubmit(model.find)
                    .accessibilityLabel("Text to find in the opened file")
                    .help("Literal text with optional Unicode lowercase matching. Up to 4096 UTF-8 bytes; NUL is unsupported.")
                Picker("Scan up to", selection: $model.searchScanBytes) {
                    Text("16 MiB").tag(UInt64(16 * 1024 * 1024))
                    Text("256 MiB").tag(AtlasObservedReaderSearchLimits.scanBytes)
                    Text("1 GiB").tag(UInt64(1024 * 1024 * 1024))
                    Text("4 GiB").tag(AtlasObservedReaderSearchLimits.maximumScanBytes)
                }.frame(width: 185)
                Button(model.searching ? "Restart Find" : "Find", action: model.find).disabled(!model.canSearch)
                Button("Previous match", action: model.previousSearchHit).disabled(!model.canSelectSearchHit)
                Button("Next match", action: model.nextSearchHit).disabled(!model.canSelectSearchHit)
            }
            HStack(spacing: 7) {
                Toggle("Match case", isOn: $model.searchCaseSensitive).toggleStyle(.checkbox)
                    .help("When off, use Unicode lowercase matching and retain the original spelling and byte ranges")
                Toggle("Count all matches", isOn: $model.countAllSearchMatches).toggleStyle(.checkbox)
                    .help("Continue counting after the first 256 retained matches, within the selected scan and read-call limits")
                Text(model.countAllSearchMatches
                    ? "Keep the first 256 matches but continue counting. The count is exact only when the observation completes unchanged."
                    : "Find stops at the first match beyond the retained-result limit.")
                    .font(.caption2).foregroundStyle(.secondary)
            }
            Text("Find starts at byte 0 using the selected decoding, with at most 256 retained matches. A scan limit or file change leaves the total count unknown.")
                .font(.caption2).foregroundStyle(.secondary)
            if let report = model.searchReport {
                HStack(alignment: .top, spacing: 12) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(report.countLabel).font(.caption.bold())
                        if !report.hits.isEmpty {
                            ScrollView {
                                LazyVStack(alignment: .leading, spacing: 2) {
                                    ForEach(report.hits) { hit in
                                        Button {
                                            model.selectSearchHit(report.target(for: hit))
                                        } label: {
                                            HStack {
                                                Image(systemName: model.selectedSearchWitness?.target == report.target(for: hit)
                                                    ? "checkmark.circle.fill" : "text.magnifyingglass")
                                                Text("\(hit.index + 1). bytes \(hit.range.start)..<\(hit.range.end)")
                                                    .font(.system(.caption, design: .monospaced))
                                            }
                                        }
                                        .buttonStyle(.plain).disabled(!model.canSelectSearchHit)
                                        .help("Verify the matching literal retained by this search, without reading the file again")
                                    }
                                }.frame(maxWidth: .infinity, alignment: .leading)
                            }.frame(minWidth: 230, maxHeight: 115)
                        }
                    }.frame(minWidth: 230, alignment: .leading)
                    if let witness = model.selectedSearchWitness {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("Verified retained literal · \(witness.encoding.label)").font(.caption.bold())
                            ScrollView {
                                Text(verbatim: witness.text).font(.system(.caption, design: .monospaced))
                                    .textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
                            }.frame(maxHeight: 50)
                            Text(verbatim: "Original hex: " + witness.originalHex)
                                .font(.system(.caption2, design: .monospaced)).lineLimit(2).textSelection(.enabled)
                            HStack {
                                Button("Copy match") { copyWitness(hex: false) }
                                Button("Copy match hex") { copyWitness(hex: true) }
                                Button("Read current context", action: model.readSearchContext)
                                    .disabled(!model.canReadSearchContext)
                                    .help("Read a new observation around this result. Select the match only if its original bytes still agree.")
                                Button("Go to verified match", action: model.goToVerifiedMatch)
                                    .disabled(!model.canGoToVerifiedMatch)
                                    .help("Read fresh context and move only after the shared engine verifies the retained match. A changed match leaves this page intact.")
                            }.controlSize(.small)
                        }.frame(maxWidth: .infinity, alignment: .leading)
                    } else {
                        Text(report.hits.isEmpty
                            ? (report.searchComplete
                                ? "No matches in the observed input. Metadata checks did not detect a change; the scan is not an atomic snapshot."
                                : "A partial search cannot prove that the unsearched remainder has no matches.")
                            : "Select a result to verify its retained literal. Read current context is a separate explicit action.")
                            .font(.caption).foregroundStyle(.secondary)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
                Text(model.searchCoverage ?? report.coverage).font(.caption2).foregroundStyle(.secondary).textSelection(.enabled)
                if let context = model.searchContext {
                    Text(context.message).font(.caption).foregroundStyle(context.selection == nil ? .secondary : .primary)
                }
            }
        }
    }

    private func copyWitness(hex: Bool) {
        guard let witness = model.selectedSearchWitness,
              model.searchReport?.admits(witness.target) == true, !model.closed else { return }
        NSPasteboard.general.clearContents()
        copyNotice = NSPasteboard.general.setString(hex ? witness.originalHex : witness.text, forType: .string)
            ? "Copied the verified retained literal\(hex ? " as original hexadecimal bytes" : ""); no file reread."
            : "Clipboard write failed."
    }

    private func copyOriginalHex() {
        let navigation = model.navigation
        copyNotice = ""
        Task {
            guard let hex = await model.copyOriginalHex(), !model.closed,
                  model.navigation == navigation else { return }
            NSPasteboard.general.clearContents()
            copyNotice = NSPasteboard.general.setString(hex, forType: .string)
                ? "Copied this observation's original bytes as hexadecimal." : "Clipboard write failed."
        }
    }
}
