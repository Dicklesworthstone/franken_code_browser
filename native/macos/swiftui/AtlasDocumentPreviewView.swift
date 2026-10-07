import SwiftUI
import AppKit

/// Native presentation of the shared engine's logical flow and provenance.
/// Text is already rendered by FrankenMarkdown: this view parses no Markdown,
/// invents no heading slugs and grants no network or external-link access.
@MainActor struct AtlasDocumentPreviewView: View {
    @Bindable var model: AtlasDocumentPreviewModel
    @Environment(\.dismiss) private var dismiss
    @State private var shownSource: AtlasSource?
    @State private var sourceSelection: AtlasReaderSelection?
    @State private var sourceNavigation = UUID()

    var body: some View {
        VStack(spacing: 0) {
            toolbar
            Divider()
            HSplitView {
                headingBrowser.frame(minWidth: 160, idealWidth: 220, maxWidth: 320)
                VStack(spacing: 0) {
                    flowControls
                    Divider()
                    flow
                    if let source = model.source, let shownSource {
                        Divider()
                        sourceContext(source, text: shownSource).frame(minHeight: 150, idealHeight: 200, maxHeight: 280)
                    }
                }
                .frame(minWidth: 520, maxWidth: .infinity, maxHeight: .infinity)
            }
            Divider()
            VStack(alignment: .leading, spacing: 4) {
                if !model.notice.isEmpty { Text(model.notice) }
                Text(model.coverage).foregroundStyle(.secondary)
            }
            .font(.caption).textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading).padding(10)
        }
        .frame(minWidth: 850, idealWidth: 1040, minHeight: 580, idealHeight: 760)
        .onChange(of: model.source?.target, initial: true) { _, _ in installSourceContext() }
        .onDisappear { model.cancel(silent: true) }
    }

    private var toolbar: some View {
        HStack(spacing: 10) {
            Label("Markdown preview", systemImage: "doc.richtext").font(.headline)
            Text("Logical flow").font(.caption).foregroundStyle(.secondary)
            Spacer()
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
            Button("Done") { model.cancel(silent: true); dismiss() }
                .keyboardShortcut(.cancelAction)
        }
        .padding(10)
    }

    private var flowControls: some View {
        HStack(spacing: 6) {
            Button("Back", action: model.back).disabled(!model.canBack)
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
        .controlSize(.small).padding(8)
    }

    @ViewBuilder private var flow: some View {
        if let page = model.page {
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
                                Button { model.showSource(target) } label: {
                                    Image(systemName: "doc.text.magnifyingglass")
                                }
                                .buttonStyle(.borderless)
                                .disabled(!model.allows(target))
                                .accessibilityLabel("Show enclosing Markdown for flow row \(line.id + 1)")
                                .help("Show the enclosing original Markdown region; this is not a glyph-exact source span")
                                Menu {
                                    Button("Copy rendered row") { copy(target, mode: .renderedText) }
                                    Button("Copy enclosing Markdown hex") { copy(target, mode: .enclosingMarkdown) }
                                } label: { Image(systemName: "doc.on.doc") }
                                .menuStyle(.borderlessButton)
                                .fixedSize()
                                .disabled(!model.allows(target))
                                .accessibilityLabel("Copy flow row \(line.id + 1)")
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
