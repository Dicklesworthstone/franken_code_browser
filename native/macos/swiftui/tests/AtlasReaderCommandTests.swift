import AppKit
import Foundation
import SwiftUI

/// Native responder-chain regressions, with real NSTextView selection and the
/// shared NSTextField editor. Run on a physical Mac through strict RCH at the
/// reviewed revision; these tests do not qualify a full application release.
@MainActor @main struct AtlasReaderCommandTests {
    static func main() {
        _ = NSApplication.shared
        recognizesOnlyDeclaredCommands()
        findFocusPreservesCapturedSource()
        twoReadersKeepKeyboardOwnership()
        sharedFieldEditorTracksItsActualOwner()
        unavailableNavigationCannotFallThrough()
        hiddenAndDisabledReadersCannotNavigate()
        windowsAndSheetsKeepIndependentOwnership()
        detachmentRetiresCommandOwnership()
        nativeQuerySubmissionPreservesExactText()
        print("AtlasReaderCommands: 9 native focus, key-equivalent, query and isolation scenarios passed")
    }

    static func expect(_ condition: @autoclosure () -> Bool, _ message: String) {
        if !condition() {
            FileHandle.standardError.write(Data(("FAIL AtlasReaderCommands: " + message + "\n").utf8))
            exit(1)
        }
    }
    static func event(_ key: String, _ modifiers: NSEvent.ModifierFlags = .command,
                      in window: NSWindow? = nil) -> NSEvent {
        NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: modifiers,
            timestamp: 0, windowNumber: window?.windowNumber ?? 0, context: nil,
            characters: key, charactersIgnoringModifiers: key, isARepeat: false, keyCode: 0)!
    }

    @MainActor final class Reader {
        let controller = AtlasSourceReaderController()
        let commands = AtlasReaderCommands()
        let field = AtlasReaderFindTextField()
        let source = AtlasSource(path: "retained.swift", text: "first\r\nneedle 😀 e\u{301}\tlast")
        var canMove = true
        var moves: [Bool] = []
        init() {
            controller.setReaderCommands(commands)
            expect(controller.update(source: source, navigation: UUID(), selection: nil) {
                NSAttributedString(string: source.text)
            }, "fixture must install the captured source")
            field.stringValue = "needle 😀"
            field.isBezeled = true
            field.isEditable = true
            field.readerCommands = commands
            commands.attach(field: field)
            configure()
        }
        func configure(active: Bool = true) {
            commands.configure(active: active, canMove: { [weak self] in self?.canMove == true },
                move: { [weak self] in self?.moves.append($0) })
        }
        func mount(in view: NSView, x: CGFloat = 0) {
            controller.view.frame = NSRect(x: x, y: 0, width: 480, height: 240)
            field.frame = NSRect(x: x + 8, y: 250, width: 300, height: 24)
            view.addSubview(controller.view)
            view.addSubview(field)
        }
    }
    static func window(_ readers: [Reader]) -> NSWindow {
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 980, height: 300),
            styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        let view = NSView(frame: NSRect(x: 0, y: 0, width: 980, height: 300))
        window.contentView = view
        for (index, reader) in readers.enumerated() { reader.mount(in: view, x: CGFloat(index * 490)) }
        return window
    }

    static func recognizesOnlyDeclaredCommands() {
        expect(AtlasReaderCommands.command(for: event("f")) == .find, "Cmd-F")
        expect(AtlasReaderCommands.command(for: event("g")) == .next, "Cmd-G")
        expect(AtlasReaderCommands.command(for: event("G", [.command, .shift])) == .previous, "Shift-Cmd-G")
        expect(AtlasReaderCommands.command(for: event("F", [.command, .capsLock])) == .find, "Caps Lock is not another command")
        let ignored: [NSEvent.ModifierFlags] = [[], .shift, [.command, .option], [.command, .control], [.command, .shift]]
        for flags in ignored {
            expect(AtlasReaderCommands.command(for: event("f", flags)) == nil, "unrelated modifiers must pass through")
        }
        expect(AtlasReaderCommands.command(for: event("c")) == nil, "copy remains an AppKit command")
    }

    static func findFocusPreservesCapturedSource() {
        let reader = Reader(), host = window([])
        defer { host.close() }
        reader.mount(in: host.contentView!)
        let selection = NSRange(location: 7, length: 6)
        reader.controller.textView.setSelectedRange(selection)
        expect(host.makeFirstResponder(reader.controller.textView), "source accepts native focus")
        expect(!reader.controller.textView.usesFindPanel, "retained Find must not search just the displayed page")
        expect(host.performKeyEquivalent(with: event("f", in: host)), "window must deliver retained Cmd-F")
        expect(reader.field.currentEditor() === host.firstResponder, "Find targets the reader's native field")
        let editor = reader.field.currentEditor() as? NSTextView
        expect(editor?.selectedRange() == NSRange(location: 0, length: reader.field.stringValue.utf16.count),
            "Cmd-F selects the existing query for replacement")
        expect(reader.controller.textView.selectedRange() == selection, "Find focus cannot replace source selection")
        expect(reader.controller.textView.string.utf8.elementsEqual(reader.source.text.utf8), "Find cannot rewrite captured bytes")
    }

    static func twoReadersKeepKeyboardOwnership() {
        let left = Reader(), right = Reader(), host = window([left, right])
        defer { host.close() }
        expect(host.makeFirstResponder(right.controller.textView), "right reader accepts native focus")
        expect(host.performKeyEquivalent(with: event("g", in: host)), "window must deliver Cmd-G with no workspace search")
        expect(host.performKeyEquivalent(with: event("g", [.command, .shift], in: host)), "window must deliver previous match")
        expect(AtlasReaderCommands.perform(.next, in: host), "toolbar uses the same native focus owner")
        // The intentional failure demands the wrong pin move. The shell runner
        // requires a failing status, proving this isolation oracle is live.
        let negative = CommandLine.arguments.contains("--negative-control")
        expect(negative ? left.moves == [false, true, false] : right.moves == [false, true, false],
            "all three commands belong to the focused pin")
        expect(left.moves.isEmpty, "a second visible reader must remain unchanged")
    }

    static func sharedFieldEditorTracksItsActualOwner() {
        let left = Reader(), right = Reader(), host = window([left, right])
        defer { host.close() }
        left.field.selectText(nil)
        expect(AtlasReaderCommands.perform(.next, in: host), "left Find owns its editor")
        right.field.selectText(nil)
        expect(host.performKeyEquivalent(with: event("g", in: host)), "right Find receives its own command")
        expect(left.moves == [false] && right.moves == [false], "shared field editor must not retain the previous owner")
        let other = NSTextField(frame: NSRect(x: 0, y: 280, width: 180, height: 20))
        host.contentView!.addSubview(other)
        other.selectText(nil)
        expect(!AtlasReaderCommands.perform(.find, in: host), "unrelated query or line fields retain their own shortcuts")
        expect(!AtlasReaderCommands.perform(.next, in: host), "unrelated fields cannot navigate retained matches")
    }

    static func unavailableNavigationCannotFallThrough() {
        let reader = Reader(), host = window([reader])
        defer { host.close() }
        reader.canMove = false
        expect(host.makeFirstResponder(reader.controller.textView), "reader accepts focus")
        expect(AtlasReaderCommands.perform(.next, in: host), "pending, stale and empty Find still own Cmd-G")
        expect(host.performKeyEquivalent(with: event("g", [.command, .shift], in: host)), "unavailable previous stays local")
        expect(reader.moves.isEmpty, "unavailable navigation cannot use old accepted matches")
        reader.canMove = true
        expect(AtlasReaderCommands.perform(.next, in: host), "current availability is checked at activation")
        expect(reader.moves == [false], "new accepted matches become navigable")
    }

    static func hiddenAndDisabledReadersCannotNavigate() {
        let reader = Reader(), host = window([reader])
        defer { host.close() }
        expect(host.makeFirstResponder(reader.controller.textView), "reader accepts focus")
        reader.configure(active: false)
        expect(AtlasReaderCommands.perform(.next, in: host), "a popover or inactive history suspends reader commands")
        reader.configure()
        reader.field.isEnabled = false
        expect(AtlasReaderCommands.perform(.next, in: host), "disabled native Find cannot dispatch")
        reader.field.isEnabled = true
        reader.controller.view.isHidden = true
        AtlasReaderCommands.perform(.next, in: host)
        expect(reader.moves.isEmpty, "suspended readers never navigate behind another presentation")
    }

    static func windowsAndSheetsKeepIndependentOwnership() {
        let parentReader = Reader(), sheetReader = Reader()
        let parent = window([parentReader]), sheet = window([sheetReader])
        defer { parent.close(); sheet.close() }
        expect(parent.makeFirstResponder(parentReader.controller.textView), "parent reader accepts focus")
        expect(sheet.makeFirstResponder(sheetReader.controller.textView), "other window accepts focus")
        expect(!parentReader.commands.perform(.next, in: sheet), "a reader cannot target another window")
        expect(AtlasReaderCommands.perform(.next, in: sheet), "the other window resolves its own reader")
        expect(parentReader.moves.isEmpty && sheetReader.moves == [false], "window routes stay independent")
        parent.beginSheet(sheet)
        expect(parent.attachedSheet === sheet, "native sheet must attach for the modal oracle")
        parentReader.commands.perform(.next, in: parent)
        expect(parentReader.moves.isEmpty, "modal presentation cannot navigate its underlying reader")
        expect(sheet.makeFirstResponder(sheetReader.controller.textView), "sheet reader accepts focus")
        expect(AtlasReaderCommands.perform(.previous, in: sheet), "retained reader inside a sheet remains usable")
        expect(sheetReader.moves == [false, true], "sheet command stays in the sheet")
        parent.endSheet(sheet)
    }

    static func detachmentRetiresCommandOwnership() {
        let reader = Reader(), host = window([reader])
        defer { host.close() }
        expect(host.makeFirstResponder(reader.controller.textView), "reader accepts focus")
        reader.controller.setReaderCommands(nil)
        reader.commands.detach(field: reader.field)
        expect(!AtlasReaderCommands.perform(.next, in: host), "dismantled reader must lose ownership")
        expect(reader.controller.textView.usesFindPanel, "unbound source keeps standard AppKit Find")
        weak var released: AtlasReaderCommands?
        do {
            let temporary = AtlasReaderCommands()
            released = temporary
            temporary.attach(field: reader.field)
        }
        expect(released == nil, "command registry must not retain closed readers")

        weak var capturedModel: NSObject?
        do {
            let model = NSObject()
            capturedModel = model
            reader.commands.attach(field: reader.field)
            reader.commands.configure(active: true, canMove: { _ = model; return true },
                move: { _ in _ = model })
        }
        expect(capturedModel != nil, "active command closures retain their model")
        reader.commands.detach(field: reader.field)
        expect(capturedModel == nil, "dismantling the field releases captured model closures")
    }

    static func nativeQuerySubmissionPreservesExactText() {
        var query = "", submissions = 0
        let commands = AtlasReaderCommands(), field = AtlasReaderFindTextField()
        let representation = AtlasReaderFindField(text: Binding(get: { query }, set: { query = $0 }),
            commands: commands, active: true, canMove: { false }, move: { _ in }, submit: { submissions += 1 })
        let coordinator = AtlasReaderFindField.Coordinator(representation)
        field.stringValue = "e\u{301} 😀\t"
        coordinator.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification, object: field))
        expect(query.utf8.elementsEqual(field.stringValue.utf8), "native query edits preserve exact spelling")
        expect(submissions == 0, "editing does not start an unrequested search")
        field.target = coordinator
        field.action = #selector(AtlasReaderFindField.Coordinator.submitQuery(_:))
        expect(field.sendAction(field.action, to: field.target), "native Return action must reach the coordinator")
        expect(submissions == 1, "the native field action submits the current exact query once")
        field.isEnabled = false
        coordinator.submitQuery(field)
        expect(submissions == 1, "disabled Find cannot submit")
    }
}
