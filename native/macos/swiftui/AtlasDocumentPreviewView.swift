import SwiftUI
import AppKit

/// Native presentation of the shared engine's shaped glyphs, logical flow and
/// provenance. This view parses no Markdown, invents no heading slugs and grants
/// no network or external-link access.
@MainActor struct AtlasDocumentPreviewView: View {
    @Bindable var model: AtlasDocumentPreviewModel
    var embedded = false
    var openSource: AtlasDocumentPreviewModel.SourceNavigation?
    var openLocalLink: AtlasDocumentPreviewModel.LocalNavigation?
    var closePreview: (() -> Void)?
    @Environment(\.dismiss) private var dismiss
    @State private var showsContents = false
    @State private var browsesLinks = false
    @State private var shownSource: AtlasSource?
    @State private var sourceSelection: AtlasReaderSelection?
    @State private var sourceNavigation = UUID()

    var body: some View {
        VStack(spacing: 0) {
            toolbar
            Divider()
            HSplitView {
                if !embedded || showsContents {
                    documentNavigation.frame(minWidth: 180, idealWidth: 250, maxWidth: 340)
                }
                VStack(spacing: 0) {
                    flowControls
                    Divider()
                    flow
                    if let source = model.source, let shownSource {
                        Divider()
                        sourceContext(source, text: shownSource).frame(minHeight: 150, idealHeight: 200, maxHeight: 280)
                    }
                }
                .frame(minWidth: embedded ? 440 : 520, maxWidth: .infinity, maxHeight: .infinity)
            }
            Divider()
            VStack(alignment: .leading, spacing: 4) {
                if !model.notice.isEmpty { Text(model.notice) }
                Text(model.coverage).foregroundStyle(.secondary)
            }
            .font(.caption).textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading).padding(10)
        }
        .frame(minWidth: embedded ? 480 : 850, idealWidth: embedded ? 560 : 1040,
            minHeight: embedded ? 300 : 580, idealHeight: 760)
        .onChange(of: model.source?.target, initial: true) { _, _ in installSourceContext() }
        .onDisappear { model.cancel(silent: true) }
    }

    private var toolbar: some View {
        HStack(spacing: 10) {
            Label("Markdown preview", systemImage: "doc.richtext").font(.headline)
            if model.richSupported && model.page != nil {
                Picker("Preview presentation", selection: Binding(get: { model.showsStyled },
                    set: { model.selectStyled($0) })) {
                    Text("Styled").tag(true)
                    Text("Text").tag(false)
                }
                .pickerStyle(.segmented).frame(width: 120).disabled(!model.canNavigate)
            }
            Spacer()
            if embedded {
                Button(showsContents ? "Hide contents" : "Contents") { showsContents.toggle() }
            }
            TextField("Columns", text: $model.widthInput)
                .frame(width: 64).textFieldStyle(.roundedBorder)
                .accessibilityLabel("Markdown preview width in columns")
                .onSubmit(model.prepare)
            Button(model.page == nil ? "Prepare" : "Reflow", action: model.prepare)
                .disabled(!model.canPrepare)
                .help("Reflow the same captured source; old preview actions are revoked until preparation succeeds")
            if model.busy {
                ProgressView().controlSize(.small).accessibilityLabel("Processing retained Markdown")
                Button("Cancel") { model.cancel() }
            }
            if embedded {
                Button("Hide") { model.cancel(silent: true); closePreview?() }
                    .accessibilityLabel("Hide the split Markdown preview; keep the source reader open")
            } else {
                Button("Done") { model.cancel(silent: true); dismiss() }
                    .keyboardShortcut(.cancelAction)
            }
        }
        .padding(10)
    }

    private var flowControls: some View {
        HStack(spacing: 6) {
            if model.showsStyled {
                Button("Previous page", action: model.previousDisplay).disabled(!model.canDisplayBack)
                Button("Next page", action: model.nextDisplay).disabled(!model.canDisplayNext)
                if let page = model.displayPage {
                    Text("Elements \(page.first + (page.items.isEmpty ? 0 : 1))–\(page.first + UInt64(page.items.count))")
                        .font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
                Button("From source page", action: model.fromSource).disabled(!model.canNavigate)
                    .help("Locate this source page in the exact logical preview")
            } else {
            Button("Back", action: model.back).disabled(!model.canBack)
            Button("Forward", action: model.forward).disabled(!model.canForward)
                .help("Revisit the document page left by Back, within this prepared generation")
            Button("Next page", action: model.next).disabled(!model.canNext)
            TextField("Flow row", text: $model.lineInput)
                .frame(width: 70).textFieldStyle(.roundedBorder)
                .accessibilityLabel("Rendered flow row, one based; not a source line")
                .onSubmit(model.goToLine)
            Button("Go", action: model.goToLine).disabled(!model.canNavigate)
            Spacer()
            Button("From source page", action: model.fromSource).disabled(!model.canNavigate)
                .help("Locate the current source reader's original byte anchor in this document")
            }
        }
        .controlSize(.small).padding(8)
    }

    @ViewBuilder private var flow: some View {
        if model.showsStyled, let page = model.displayPage {
            AtlasDocumentDisplayView(page: page, fonts: model.displayFonts, enabled: model.canNavigate) { item in
                model.showDisplaySource(item, pageID: page.id, navigate: embedded ? openSource : nil)
            }
        } else if let page = model.page {
            ScrollView([.horizontal, .vertical]) {
                VStack(alignment: .leading, spacing: 2) {
                    if page.lines.isEmpty {
                        Text("This document has no rendered rows.").foregroundStyle(.secondary)
                    }
                    ForEach(page.lines) { line in
                        HStack(alignment: .top, spacing: 10) {
                            Text(String(line.id + 1))
                                .font(.system(size: 10, design: .monospaced)).foregroundStyle(.secondary)
                                .frame(width: 38, alignment: .trailing).accessibilityHidden(true)
                            Text(verbatim: line.text.isEmpty ? " " : line.text)
                                .font(.system(size: 13, design: .monospaced))
                                .textSelection(.enabled)
                                .fixedSize(horizontal: true, vertical: true)
                                .frame(maxWidth: .infinity, alignment: .leading)
                            if let target = page.target(at: line.id) {
                                Button { model.showSource(target, navigate: embedded ? openSource : nil) } label: {
                                    Image(systemName: "doc.text.magnifyingglass")
                                }
                                .buttonStyle(.borderless)
                                .disabled(!model.allows(target))
                                .accessibilityLabel("Show enclosing Markdown for flow row \(line.id + 1)")
                                .help("Show the enclosing original Markdown region; in split mode also navigate the retained source reader")
                                Menu {
                                    if let openSource {
                                        Button("Open in source reader") { model.showSource(target, navigate: openSource) }
                                    }
                                    Button("Copy rendered row") { copy(target, mode: .renderedText) }
                                    Button("Copy enclosing Markdown hex") { copy(target, mode: .enclosingMarkdown) }
                                } label: { Image(systemName: "doc.on.doc") }
                                .menuStyle(.borderlessButton)
                                .fixedSize()
                                .disabled(!model.allows(target))
                                .accessibilityLabel("Copy or open flow row \(line.id + 1)")
                            } else {
                                Text("Unmapped").font(.caption2).foregroundStyle(.secondary)
                                    .help("This synthetic flow row has no original source region to activate")
                            }
                        }
                    }
                }
                .padding(10)
            }
            .id(page.id)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            VStack(spacing: 12) {
                Image(systemName: "doc.richtext").font(.largeTitle)
                Text(model.busy ? "Preparing retained Markdown…" : "Markdown preview is not prepared")
                if let reason = model.unavailableReason {
                    Text(reason).font(.callout).foregroundStyle(.secondary).multilineTextAlignment(.center)
                }
                if !model.busy && model.canPrepare { Button("Prepare preview", action: model.prepare) }
            }
            .padding(20).frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private var documentNavigation: some View {
        VStack(spacing: 0) {
            Picker("Document navigation", selection: $browsesLinks) {
                Text("Headings").tag(false)
                Text("Links").tag(true)
            }
            .pickerStyle(.segmented).padding(8)
            if browsesLinks { linkBrowser } else { headingBrowser }
        }
    }

    private var linkBrowser: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("Links").font(.headline)
                Spacer()
                Button(model.links == nil ? "Load links" : "Reload", action: model.prepareLinks)
                    .disabled(!model.canNavigate)
                    .help("Parse link occurrences from this exact captured Markdown document")
            }
            .padding(.horizontal, 10)
            HStack {
                Button("Previous", action: model.previousLinks).disabled(!model.canLinksBack)
                Button("Next", action: model.nextLinks).disabled(!model.canLinksNext)
            }
            .controlSize(.small).padding(.horizontal, 10)
            List {
                if let links = model.links {
                    ForEach(links.rows) { link in
                        if let target = links.target(id: link.id) {
                            Button { model.openLink(target, openLocal: openLocalLink) } label: {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(verbatim: link.display.isEmpty ? "(document start)" : link.display)
                                        .lineLimit(3).font(.system(.body, design: .monospaced))
                                    Text("Occurrence \(link.id) · " + linkDisposition(link.kind))
                                        .font(.caption).foregroundStyle(.secondary)
                                }
                                .frame(maxWidth: .infinity, alignment: .leading).contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .disabled(!model.canNavigate || !(link.kind.navigatesWithinCapture
                                || (link.kind == .local && openLocalLink != nil)))
                            .help(linkDisposition(link.kind) + ": " + link.display)
                        }
                    }
                    if links.rows.isEmpty {
                        Text(links.complete && links.total == 0 ? "No links in this capture" : "No retained links on this page")
                            .foregroundStyle(.secondary)
                    }
                }
            }
            .listStyle(.sidebar)
            Text(model.linkCoverage).font(.caption).foregroundStyle(.secondary)
                .textSelection(.enabled).padding(10)
        }
    }
    private func linkDisposition(_ kind: AtlasReaderDocumentLinkKind) -> String {
        switch kind {
        case .heading: "Go to this document's heading"
        case .start: "Go to document start"
        case .local: openLocalLink == nil ? "Local target needs a project capture route" : "Open admitted project target"
        case .external: "External destination; not opened"
        case .unsupported: "Unsupported destination"
        }
    }

    private var headingBrowser: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("Headings").font(.headline)
                Spacer()
                if let headings = model.headings { Text(String(headings.inventory.totalHeadings)).foregroundStyle(.secondary) }
            }
            .padding(.horizontal, 10).padding(.top, 10)
            HStack {
                Button("Previous", action: model.previousHeadings).disabled(!model.canHeadingsBack)
                Button("Next", action: model.nextHeadings).disabled(!model.canHeadingsNext)
            }
            .controlSize(.small).padding(.horizontal, 10)
            List {
                if let headings = model.headings {
                    ForEach(headings.rows) { heading in
                        if let target = headings.target(slug: heading.slug) {
                            Button { model.openHeading(target) } label: {
                                Text(verbatim: heading.title).lineLimit(3)
                                    .frame(maxWidth: .infinity, alignment: .leading).contentShape(Rectangle())
                            }
                            .buttonStyle(.plain).disabled(!model.canNavigate)
                            .help("Jump to the engine's retained heading: " + heading.slug)
                        }
                    }
                    if headings.rows.isEmpty { Text("No headings").foregroundStyle(.secondary) }
                }
            }
            .listStyle(.sidebar)
        }
    }

    private func sourceContext(_ source: AtlasReaderDocumentSource, text: AtlasSource) -> some View {
        VStack(spacing: 0) {
            HStack {
                Text("Enclosing original Markdown · bytes \(source.enclosingOriginal.start)..<\(source.enclosingOriginal.end)")
                    .font(.system(size: 11, design: .monospaced))
                Spacer()
                if let openSource {
                    Button("Open in source reader") { openSource(source.target) }
                        .disabled(!model.allows(source.target))
                        .help("Navigate within this same capture; do not reopen the live file")
                }
                Button("Copy region hex") { copy(source.target, mode: .enclosingMarkdown) }
                    .disabled(!model.allows(source.target))
            }
            .controlSize(.small).padding(8)
            AtlasSourceReader(source: text, navigation: sourceNavigation, selection: sourceSelection) {
                NSAttributedString(string: text.text, attributes: [
                    .font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular),
                    .foregroundColor: NSColor.textColor
                ])
            }
        }
    }

    private func installSourceContext() {
        shownSource = nil; sourceSelection = nil
        sourceNavigation = UUID()
        guard let source = model.source else { return }
        // Window-local UTF-8 and original capture bytes remain distinct. This
        // uses the same checked UTF-8 -> UTF-16 mapping as the source reader.
        let text = AtlasSource(path: source.target.inventory.identity.displayPath, text: source.text)
        guard let range = text.utf16Range(byteStart: source.selectionUTF8.start, byteEnd: source.selectionUTF8.end) else { return }
        shownSource = text
        sourceSelection = AtlasReaderSelection(source: text, range: range)
    }
    private func copy(_ target: AtlasReaderDocumentTarget, mode: AtlasReaderDocumentCopyMode) {
        model.copy(target, mode: mode) { text in
            NSPasteboard.general.clearContents()
            return NSPasteboard.general.setString(text, forType: .string)
        }
    }
}
