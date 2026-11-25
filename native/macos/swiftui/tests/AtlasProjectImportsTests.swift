import Foundation

@main private enum AtlasProjectImportsTests {
    @MainActor private static func settled(_ model: AtlasProjectLinksModel) async {
        let deadline = ProcessInfo.processInfo.systemUptime + 10
        while model.busy && ProcessInfo.processInfo.systemUptime < deadline { await Task.yield() }
        precondition(!model.busy, "Relationship work did not drain")
    }
    private static func prepared(_ fixture: ProjectLinksFixture) async throws -> AtlasPreparedIndex {
        let worker = AtlasIndexedSearch(transport: fixture.index.transport)
        return try await Task.detached {
            try worker.prepare(root: "/project", cancellation: AtlasSearchCancellation(),
                access: AtlasSearchAccessLease(nil), progress: { _ in })
        }.value
    }
    private static func decode(_ value: [String: Any], index: AtlasPreparedIndex,
        direction: AtlasProjectLinkDirection = .all, path: [UInt8]? = nil) throws -> AtlasProjectLinksPage {
        try AtlasProjectLinksPage.decode(ProjectLinksFixture.json(value), index: index, generation: 1,
            command: "imports-page", direction: direction, path: path, projection: .imports)
    }
    private static func refuse(_ value: [String: Any], index: AtlasPreparedIndex) {
        do { _ = try decode(value, index: index); preconditionFailure("Malformed import authority admitted") }
        catch {}
    }
    private static func wire(_ index: AtlasPreparedIndex) throws {
        let valid = ProjectLinksFixture.importPage(owner: index.basis.owner, generation: 1, command: "imports-page")
        let page = try decode(valid, index: index)
        precondition(page.projection == .imports && page.complete && page.total == 70 && page.next == 64)
        let link = page.links[0]
        precondition(link.source.id == 3 && link.target?.id == 4 && link.block == nil
            && link.evidenceRange.start == 11 && link.evidenceRange.end == 21
            && link.evidenceLabel.contains("not compiler resolved") && link.canOpenTarget)
        precondition(page.summary.contains("does not establish compiler"), "Lexical completeness must stay qualified")
        let incoming = ProjectLinksFixture.importPage(owner: index.basis.owner, generation: 1,
            command: "imports-page", direction: .incoming, path: "src/child.rs")
        let incomingPage = try decode(incoming, index: index, direction: .incoming, path: Array("src/child.rs".utf8))
        precondition(incomingPage.total == 70 && incomingPage.filterFile == 4)

        for (key, value) in [
            ("schema", "fcb.atlas-links/1"), ("command", "links-page"), ("relationship_generation", "2"),
            ("projection", "compiler-imports"), ("relationship_kind", "parsed-markdown-link"),
            ("evidence_precision", "whole-captured-markdown-document"), ("extractor", "unknown"),
            ("resolver", "compiler"), ("extraction_policy", "all-languages"),
            ("extractor", "franken-markdown-d6fbd1d5/import-statements-v9"),
            ("extraction_policy", "rust-file-includes-use-tree-js-literal-loads-python-cpp-go-java-v9"),
            ("resolver", "fcb-closed-catalog-imports-v11"),
            ("source_bytes_read", "1"), ("read_calls", "1"), ("max_source_bytes", "131072"),
            ("max_source_bytes", "262144"),
            ("pending_source_files", "1"), ("retained_links", "71")
        ] {
            var bad = valid; bad[key] = value; refuse(bad, index: index)
        }
        for key in ["compiler_resolved", "semantic_complete"] {
            var bad = valid; bad[key] = true; refuse(bad, index: index)
            bad = valid; bad.removeValue(forKey: key); refuse(bad, index: index)
        }
        for (key, value) in [
            ("language", "python"), ("lexical_import_kind", "javascript-import"), ("resolution_basis", "external-specifier"),
            ("source_bytes", "1048577"), ("source_file_id", "5"), ("source_link_id", "0"), ("destination", "")
        ] {
            var bad = valid, rows = valid["links"] as! [[String: Any]]
            rows[0][key] = value; bad["links"] = rows; refuse(bad, index: index)
        }
        for range in [["start": "21", "end": "11"], ["start": "11", "end": "11"], ["start": "11", "end": "4097"]] {
            var bad = valid, rows = valid["links"] as! [[String: Any]]
            rows[0]["evidence_range"] = range; bad["links"] = rows; refuse(bad, index: index)
        }
        var markdown = valid, rows = valid["links"] as! [[String: Any]]
        rows[0]["block_index"] = "0"; markdown["links"] = rows; refuse(markdown, index: index)

        for (resolution, basis) in [
            ("unresolved", "unresolved-rust-use"), ("external", "external-specifier"),
            ("missing", "no-catalog-target"), ("ambiguous", "ambiguous-catalog-target")
        ] {
            var value = valid, rows = valid["links"] as! [[String: Any]]
            for i in rows.indices {
                rows[i]["resolution"] = resolution; rows[i]["resolution_basis"] = basis
                for key in ["target_file_id", "target_path", "target_revision", "target_sha256", "target_bytes"] { rows[i][key] = NSNull() }
            }
            value["links"] = rows
            let admitted = try decode(value, index: index).links[0]
            precondition(!admitted.canOpenTarget && !admitted.evidenceLabel.isEmpty,
                "Unresolved imports retain exact readable evidence without manufacturing a target")
        }

        for side in [AtlasProjectLinkSide.evidence, .target] {
            let value = ProjectLinksFixture.importReceipt(owner: index.basis.owner, reader: 77,
                generation: 1, link: 1, side: side)
            func admit(_ value: [String: Any]) throws -> AtlasProjectLinkReaderReceipt {
                try AtlasProjectLinkReaderReceipt.decode(ProjectLinksFixture.json(value), index: index,
                    generation: 1, link: link, side: side, destination: 77)
            }
            func reject(_ value: [String: Any]) {
                do { _ = try admit(value); preconditionFailure("Changed import source evidence admitted") } catch {}
            }
            let receipt = try admit(value)
            precondition(receipt.initialOriginalOffset == (side == .evidence ? 11 : 0) && receipt.fragment == nil)
            precondition(receipt.info.identity.pathBytes == (side == .evidence ? link.source.path.bytes : link.target!.path.bytes))
            for (key, changed) in [
                ("initial_original_offset", "12"), ("source_sha256", String(repeating: "f", count: 64)),
                ("source_revision", "2"), ("reader_owner", "78"), ("additional_source_bytes_read", "1"),
                ("side", side == .evidence ? "target" : "evidence")
            ] {
                var bad = value; bad[key] = changed; reject(bad)
            }
            var bad = value; bad["source_reopened"] = true; reject(bad)
            bad = value; bad["compiler_resolved"] = true; reject(bad)
            bad = value; bad["evidence_range"] = ["start": "12", "end": "21"]; reject(bad)
            bad = value; bad["fragment_status"] = "resolved"; reject(bad)
            for key in ["destination", "source_link_id", "source_sha256", "resolution_basis"] {
                bad = value; var row = value["relationship"] as! [String: Any]
                row[key] = key == "source_link_id" ? "2" : "changed"
                bad["relationship"] = row; reject(bad)
            }
        }

        // Inline-module context is resolved by the engine and has no new wire
        // authority. Nested targets retain the exact source identity and range.
        for (destination, kind, basis) in [
            ("child", "rust-module", "rust-module-file-candidate"),
            ("self::child::Thing", "rust-use", "rust-use-module-prefix-candidate"),
            ("super::child::Thing", "rust-use", "rust-use-module-prefix-candidate")
        ] {
            var nested = valid, nestedRows = valid["links"] as! [[String: Any]]
            for i in nestedRows.indices {
                nestedRows[i]["destination"] = destination; nestedRows[i]["lexical_import_kind"] = kind
                nestedRows[i]["resolution_basis"] = basis
                nestedRows[i]["target_path"] = ProjectLinksFixture.path("src/outer/child.rs")
            }
            nested["links"] = nestedRows
            let candidate = try decode(nested, index: index).links[0]
            precondition(candidate.canOpenTarget && candidate.importEvidence?.kind == kind)
            precondition(candidate.target?.path.bytes == Array("src/outer/child.rs".utf8))
            precondition(candidate.source == link.source && candidate.evidenceRange == link.evidenceRange,
                "Nested module resolution must not replace retained source evidence")
        }

        // The other projection's envelope cannot be decoded by changing a command.
        var substituted = ProjectLinksFixture.page(owner: index.basis.owner, generation: 1, command: "imports-page")
        substituted["schema"] = "fcb.atlas-imports/1"; refuse(substituted, index: index)
    }


    /// Language support is accepted only with its declared evidence policy.
    /// Scripted envelopes exercise the native consumer; real-source extraction
    /// and original-byte navigation are covered by the Rust host/ABI fixtures.
    private static func pythonWire(_ index: AtlasPreparedIndex) throws {
        var value = ProjectLinksFixture.importPage(owner: index.basis.owner,
            generation: 1, command: "imports-page")
        var rows = value["links"] as! [[String: Any]]
        for i in rows.indices {
            rows[i]["language"] = "python"
            rows[i]["lexical_import_kind"] = "python-from"
            rows[i]["destination"] = ".worker"
            rows[i]["resolution_basis"] = "python-relative-module-candidate"
            rows[i]["source_path"] = ProjectLinksFixture.path("pkg/main.py")
            rows[i]["target_path"] = ProjectLinksFixture.path("pkg/worker.py")
        }
        value["links"] = rows
        let page = try decode(value, index: index), link = page.links[0]
        precondition(link.canOpenTarget && link.importEvidence?.language == "python")
        precondition(link.importEvidence?.kind == "python-from"
            && link.evidenceLabel.contains("Python from-import statement")
            && link.evidenceLabel.contains("not compiler resolved"))
        precondition(link.source.path.bytes == Array("pkg/main.py".utf8)
            && link.target?.path.bytes == Array("pkg/worker.py".utf8))

        for (key, wrong) in [
            ("language", "rust"), ("language", "swift"),
            ("lexical_import_kind", "python-import"),
            ("lexical_import_kind", "javascript-import"),
            ("resolution_basis", "rust-module-file-candidate"),
            ("resolution_basis", "unique-relative-module-candidate"),
            ("resolution_basis", "external-specifier")
        ] {
            var bad = value, changed = rows
            changed[0][key] = wrong; bad["links"] = changed
            refuse(bad, index: index)
        }
        for (key, oldPolicy) in [
            ("extractor", "franken-markdown-d6fbd1d5/import-statements-v1"),
            ("resolver", "fcb-closed-catalog-imports-v1"),
            ("extraction_policy", "rust-mod-use-and-js-static-import-export-v1")
        ] {
            var bad = value; bad[key] = oldPolicy
            refuse(bad, index: index)
        }

        for kind in ["python-import", "python-from"] {
            var candidate = value, candidateRows = rows
            for i in candidateRows.indices {
                candidateRows[i]["lexical_import_kind"] = kind
                candidateRows[i]["destination"] = "pkg.worker"
                candidateRows[i]["resolution_basis"] = "python-absolute-module-candidate"
                candidateRows[i]["target_path"] = ProjectLinksFixture.path("src/pkg/worker.py")
            }
            candidate["links"] = candidateRows
            let admitted = try decode(candidate, index: index).links[0]
            precondition(admitted.canOpenTarget && admitted.importEvidence?.kind == kind)
            precondition(admitted.target?.path.bytes == Array("src/pkg/worker.py".utf8)
                && admitted.label.contains("runtime import binding is not resolved"))
            for (key, wrong) in [("language", "rust"), ("lexical_import_kind", "rust-use"),
                ("destination", ".worker"), ("resolution_basis", "external-specifier")] {
                var bad = candidate, changed = candidateRows
                changed[0][key] = wrong; bad["links"] = changed; refuse(bad, index: index)
            }
            for resolution in ["external", "missing", "ambiguous", "unresolved"] {
                var bad = candidate, changed = candidateRows
                changed[0]["resolution"] = resolution
                for key in ["target_file_id", "target_path", "target_revision", "target_sha256", "target_bytes"] {
                    changed[0][key] = NSNull()
                }
                bad["links"] = changed; refuse(bad, index: index)
            }
            var old = candidate; old["resolver"] = "fcb-closed-catalog-imports-v7"
            refuse(old, index: index)
        }

        // Absolute names without a declared catalog candidate retain readable
        // evidence without granting runtime package or sys.path authority.
        var external = value, externalRows = rows
        for i in externalRows.indices {
            externalRows[i]["lexical_import_kind"] = "python-import"
            externalRows[i]["destination"] = "external.library"
            externalRows[i]["resolution"] = "external"
            externalRows[i]["resolution_basis"] = "external-specifier"
            for key in ["target_file_id", "target_path", "target_revision", "target_sha256", "target_bytes"] {
                externalRows[i][key] = NSNull()
            }
        }
        external["links"] = externalRows
        let absolute = try decode(external, index: index).links[0]
        precondition(!absolute.canOpenTarget && absolute.importEvidence?.kind == "python-import"
            && absolute.evidenceLabel.contains("Python import statement"))
        for (resolution, basis) in [("missing", "no-catalog-target"),
            ("ambiguous", "ambiguous-catalog-target"), ("unresolved", "unsupported-specifier")] {
            var unavailable = external, unavailableRows = externalRows
            for i in unavailableRows.indices {
                unavailableRows[i]["lexical_import_kind"] = "python-from"
                unavailableRows[i]["destination"] = ".worker"
                unavailableRows[i]["resolution"] = resolution
                unavailableRows[i]["resolution_basis"] = basis
            }
            unavailable["links"] = unavailableRows
            let unavailablePage = try decode(unavailable, index: index)
            precondition(unavailablePage.links.allSatisfy { !$0.canOpenTarget })
        }
    }

    private static func javaWire(_ index: AtlasPreparedIndex) throws {
        let original = ProjectLinksFixture.importPage(owner: index.basis.owner,
            generation: 1, command: "imports-page")
        for (kind, destination, basis) in [
            ("java-import", "example.Tool", "java-source-file-candidate"),
            ("java-import", "example.Tool.Nested", "java-enclosing-type-candidate"),
            ("java-static-import", "example.Tool.VALUE", "java-source-file-candidate"),
            ("java-static-import", "example.Tool.*", "java-source-file-candidate")
        ] {
            var value = original, rows = original["links"] as! [[String: Any]]
            for i in rows.indices {
                rows[i]["language"] = "java"; rows[i]["lexical_import_kind"] = kind
                rows[i]["destination"] = destination; rows[i]["resolution_basis"] = basis
                rows[i]["source_path"] = ProjectLinksFixture.path("src/main/java/app/Main.java")
                rows[i]["target_path"] = ProjectLinksFixture.path("src/main/java/example/Tool.java")
            }
            value["links"] = rows
            let link = try decode(value, index: index).links[0]
            precondition(link.canOpenTarget && link.importEvidence?.language == "java"
                && link.evidenceLabel.contains("not compiler resolved")
                && link.label.contains("classpath are not resolved"))
            precondition(link.evidenceLabel.contains(kind == "java-import" ? "Java type import" : "Java static member import"))
            if basis == "java-enclosing-type-candidate" { precondition(link.label.contains("enclosing Java type candidate")) }
            for (key, wrong) in [("language", "csharp"), ("lexical_import_kind", "go-import"),
                ("resolution_basis", "go-module-package-file-candidate"), ("resolution_basis", "external-specifier"),
                ("resolution_basis", "unresolved-java-wildcard"), ("resolution_basis", "ambiguous-catalog-target")] {
                var bad = value, changed = rows
                changed[0][key] = wrong; bad["links"] = changed; refuse(bad, index: index)
            }
            var wrongLanguage = value, changed = rows
            changed[0]["language"] = "rust"; changed[0]["lexical_import_kind"] = "rust-use"
            wrongLanguage["links"] = changed; refuse(wrongLanguage, index: index)
            for (key, old) in [("extractor", "franken-markdown-d6fbd1d5/import-statements-v6"),
                ("resolver", "fcb-closed-catalog-imports-v6"),
                ("extraction_policy", "rust-use-tree-js-literal-loads-python-cpp-go-v6")] {
                var bad = value; bad[key] = old; refuse(bad, index: index)
            }
            for state in ["unavailable", "uncaptured", "outside-capture-scope"] {
                var unavailable = value, changed = rows
                for i in changed.indices {
                    changed[i]["resolution"] = state
                    for key in ["target_revision", "target_sha256", "target_bytes"] { changed[i][key] = NSNull() }
                }
                unavailable["links"] = changed
                let unavailablePage = try decode(unavailable, index: index)
                precondition(unavailablePage.links.allSatisfy { !$0.canOpenTarget && $0.targetFile == 4 })
            }
        }
        for (resolution, basis) in [("unresolved", "unresolved-java-wildcard"),
            ("missing", "no-catalog-target"), ("ambiguous", "ambiguous-catalog-target")] {
            var value = original, rows = original["links"] as! [[String: Any]]
            for i in rows.indices {
                rows[i]["language"] = "java"; rows[i]["lexical_import_kind"] = "java-import"
                rows[i]["destination"] = resolution == "unresolved" ? "example.*" : "example.Tool"
                rows[i]["resolution"] = resolution; rows[i]["resolution_basis"] = basis
                for key in ["target_file_id", "target_path", "target_revision", "target_sha256", "target_bytes"] { rows[i][key] = NSNull() }
            }
            value["links"] = rows
            let edge = try decode(value, index: index).links[0]
            precondition(!edge.canOpenTarget && edge.targetFile == nil)
            if resolution == "unresolved" {
                precondition(edge.label.contains("on-demand import"))
                var staticWildcard = value, changed = rows
                changed[0]["lexical_import_kind"] = "java-static-import"
                staticWildcard["links"] = changed; refuse(staticWildcard, index: index)
            }
            rows[0]["target_file_id"] = "4"; rows[0]["target_path"] = ProjectLinksFixture.path("src/main/java/example/Tool.java")
            value["links"] = rows; refuse(value, index: index)
        }
    }

    private static func cargoDependencyWire(_ index: AtlasPreparedIndex) throws {
        for basis in ["rust-path-dependency-file-candidate", "rust-path-dependency-prefix-candidate"] {
            var value = ProjectLinksFixture.importPage(owner: index.basis.owner,
                generation: 1, command: "imports-page")
            // Repeated captured target manifests count toward the aggregate
            // parse budget, beyond the former one-manifest-per-source bound.
            value["parsed_module_bytes"] = "196608"
            var rows = value["links"] as! [[String: Any]]
            for i in rows.indices {
                rows[i]["language"] = "rust"; rows[i]["lexical_import_kind"] = "rust-use"
                rows[i]["destination"] = "dep_alias::model::Thing"
                rows[i]["resolution_basis"] = basis
                rows[i]["source_path"] = ProjectLinksFixture.path("app/src/main.rs")
                rows[i]["target_path"] = ProjectLinksFixture.path("dep/src/model.rs")
            }
            value["links"] = rows
            let page = try decode(value, index: index)
            precondition(page.links[0].canOpenTarget
                && page.links[0].label.contains("Cargo path dependency")
                && page.links[0].label.contains("compiler resolution is not evaluated"))
            for (key, wrong) in [("language", "go"), ("lexical_import_kind", "rust-module"),
                ("resolution", "external"), ("resolution_basis", "rust-dependency-context-unavailable")] {
                var bad = value, changed = rows
                changed[0][key] = wrong; bad["links"] = changed; refuse(bad, index: index)
            }
            var old = value; old["resolver"] = "fcb-closed-catalog-imports-v12"; refuse(old, index: index)
            for resolution in ["unavailable", "uncaptured", "outside-capture-scope"] {
                var withheld = value, changed = rows
                for i in changed.indices {
                    changed[i]["resolution"] = resolution
                    for key in ["target_revision", "target_sha256", "target_bytes"] { changed[i][key] = NSNull() }
                }
                withheld["links"] = changed
                let unavailable = try decode(withheld, index: index)
                precondition(!unavailable.links[0].canOpenTarget)
            }
            var unresolved = value, changed = rows
            for i in changed.indices {
                changed[i]["resolution"] = "unresolved"
                changed[i]["resolution_basis"] = "rust-dependency-context-unavailable"
                for key in ["target_file_id", "target_path", "target_revision", "target_sha256", "target_bytes"] {
                    changed[i][key] = NSNull()
                }
            }
            unresolved["links"] = changed
            let edge = try decode(unresolved, index: index).links[0]
            precondition(!edge.canOpenTarget && edge.label.contains("Cargo path dependency context"))
            changed[0]["target_file_id"] = "4"; changed[0]["target_path"] = ProjectLinksFixture.path("dep/src/model.rs")
            unresolved["links"] = changed; refuse(unresolved, index: index)
        }
    }

    private static func cargoWorkspaceMetadataWire(_ index: AtlasPreparedIndex) throws {
        var legacy = ProjectLinksFixture.importPage(owner: index.basis.owner,
            generation: 1, command: "imports-page")
        // Two scanned sources and 70 retained uses admit 72 manifests in v13.
        // Workspace inheritance adds at most one root manifest per source.
        legacy["parsed_module_bytes"] = String(72 * 65_536)
        let oldPage = try decode(legacy, index: index)
        precondition(oldPage.coverage.scanned == 2 && oldPage.coverage.linksSeen == 70
            && oldPage.coverage.moduleBytes == 72 * 65_536)
        var excessLegacy = legacy
        excessLegacy["parsed_module_bytes"] = String(72 * 65_536 + 1)
        refuse(excessLegacy, index: index)

        var workspace = excessLegacy
        workspace["resolver"] = "fcb-closed-catalog-imports-v14"
        var rows = workspace["links"] as! [[String: Any]]
        for i in rows.indices {
            rows[i]["language"] = "rust"; rows[i]["lexical_import_kind"] = "rust-use"
            rows[i]["destination"] = "dep_alias::Thing"
            rows[i]["resolution_basis"] = "rust-path-dependency-prefix-candidate"
            rows[i]["source_path"] = ProjectLinksFixture.path("crates/app/src/main.rs")
            rows[i]["target_path"] = ProjectLinksFixture.path("crates/dep/src/lib.rs")
        }
        workspace["links"] = rows
        let inherited = try decode(workspace, index: index)
        precondition(inherited.links[0].canOpenTarget
            && inherited.links[0].label.contains("Cargo path dependency"))
        workspace["parsed_module_bytes"] = String(74 * 65_536)
        let fullAllowance = try decode(workspace, index: index)
        precondition(fullAllowance.coverage.moduleBytes == 74 * 65_536)
        var excess = workspace
        excess["parsed_module_bytes"] = String(74 * 65_536 + 1)
        refuse(excess, index: index)
        var unknown = workspace; unknown["resolver"] = "fcb-closed-catalog-imports-v15"
        refuse(unknown, index: index)

        // Enough retained uses make the per-manifest allowance exceed 32 MiB;
        // source and manifest parsing must still share the original total cap.
        var aggregate = workspace
        for key in ["links_seen", "retained_links", "matching_links"] { aggregate[key] = "4096" }
        let parseBytes = UInt64(aggregate["parsed_source_bytes"] as! String)!
        aggregate["parsed_module_bytes"] = String(33_554_432 - parseBytes)
        let bounded = try decode(aggregate, index: index)
        precondition(bounded.coverage.parseBytes + bounded.coverage.moduleBytes == 33_554_432)
        aggregate["parsed_module_bytes"] = String(33_554_433 - parseBytes)
        refuse(aggregate, index: index)
    }

    private static func goWire(_ index: AtlasPreparedIndex) throws {
        var value = ProjectLinksFixture.importPage(owner: index.basis.owner,
            generation: 1, command: "imports-page")
        value["parsed_module_bytes"] = "128"
        var rows = value["links"] as! [[String: Any]]
        for i in rows.indices {
            rows[i]["language"] = "go"; rows[i]["lexical_import_kind"] = "go-import"
            rows[i]["destination"] = "example.local/project/pkg"
            rows[i]["resolution_basis"] = "go-module-package-file-candidate"
            rows[i]["source_path"] = ProjectLinksFixture.path("main.go")
            rows[i]["target_path"] = ProjectLinksFixture.path("pkg/a.go")
        }
        value["links"] = rows
        let page = try decode(value, index: index)
        precondition(page.links[0].canOpenTarget && page.links[0].evidenceLabel.contains("Go package import")
            && page.links[0].label.contains("representative Go package file")
            && page.links[0].label.contains("build constraints are not evaluated"))
        for (key, wrong) in [("language", "rust"), ("lexical_import_kind", "rust-use"),
            ("resolution_basis", "python-relative-module-candidate"), ("resolution_basis", "ambiguous-catalog-target")] {
            var bad = value, changed = rows
            changed[0][key] = wrong; bad["links"] = changed; refuse(bad, index: index)
        }
        for bytes in ["4718593", "33554432", "18446744073709551615"] {
            var bad = value; bad["parsed_module_bytes"] = bytes; refuse(bad, index: index)
        }
        for (basis, label) in [("go-module-context-unavailable", "go.mod context"),
            ("go-nested-module-boundary", "nested Go module")] {
            var unresolved = value, changed = rows
            for i in changed.indices {
                changed[i]["resolution"] = "unresolved"; changed[i]["resolution_basis"] = basis
                for key in ["target_file_id", "target_path", "target_revision", "target_sha256", "target_bytes"] {
                    changed[i][key] = NSNull()
                }
            }
            unresolved["links"] = changed
            let edge = try decode(unresolved, index: index).links[0]
            precondition(!edge.canOpenTarget && edge.label.contains(label))
            changed[0]["target_file_id"] = "4"; changed[0]["target_path"] = ProjectLinksFixture.path("pkg/a.go")
            unresolved["links"] = changed; refuse(unresolved, index: index)
        }
    }

    /// The C/C++ wire policy never promotes an angle or computed header to a
    /// catalog target, even when a foreign envelope supplies target metadata.
    private static func cppWire(_ index: AtlasPreparedIndex) throws {
        var value = ProjectLinksFixture.importPage(owner: index.basis.owner,
            generation: 1, command: "imports-page")
        var rows = value["links"] as! [[String: Any]]
        for i in rows.indices {
            rows[i]["language"] = "cpp"; rows[i]["lexical_import_kind"] = "cpp-quoted-include"
            rows[i]["destination"] = "detail/value.h"
            rows[i]["resolution_basis"] = "cpp-source-relative-include-candidate"
            rows[i]["source_path"] = ProjectLinksFixture.path("src/main.cpp")
            rows[i]["target_path"] = ProjectLinksFixture.path("src/detail/value.h")
        }
        value["links"] = rows
        let link = try decode(value, index: index).links[0]
        precondition(link.canOpenTarget && link.importEvidence?.language == "cpp"
            && link.evidenceLabel.contains("Quoted C/C++ include")
            && link.evidenceLabel.contains("not compiler resolved"))
        for (key, wrong) in [("language", "rust"), ("lexical_import_kind", "cpp-angle-include"),
            ("lexical_import_kind", "cpp-computed-include"), ("resolution_basis", "unique-relative-module-candidate"),
            ("resolution_basis", "python-relative-module-candidate"), ("resolution_basis", "ambiguous-catalog-target")] {
            var bad = value, changed = rows
            changed[0][key] = wrong; bad["links"] = changed; refuse(bad, index: index)
        }
        var compiler = value; compiler["compiler_resolved"] = true; refuse(compiler, index: index)
        var oldPolicy = value; oldPolicy["resolver"] = "fcb-closed-catalog-imports-v3"
        refuse(oldPolicy, index: index)
        for resolution in ["unavailable", "uncaptured", "outside-capture-scope"] {
            var unavailable = value, changed = rows
            for i in changed.indices {
                changed[i]["resolution"] = resolution
                for key in ["target_revision", "target_sha256", "target_bytes"] { changed[i][key] = NSNull() }
            }
            unavailable["links"] = changed
            let unavailablePage = try decode(unavailable, index: index)
            precondition(unavailablePage.links.allSatisfy { !$0.canOpenTarget && $0.targetFile == 4 })
        }
        for (kind, destination, resolution, basis, label) in [
            ("cpp-angle-include", "vector", "external", "external-specifier", "External C/C++ include"),
            ("cpp-computed-include", "HEADER_NAME", "unresolved", "unresolved-cpp-include", "Unresolved computed C/C++ include"),
            ("cpp-quoted-include", "missing.h", "missing", "no-catalog-target", "Quoted C/C++ include"),
            ("cpp-quoted-include", "../escape.h", "unresolved", "unsupported-specifier", "Quoted C/C++ include")
        ] {
            var other = value, changed = rows
            for i in changed.indices {
                changed[i]["lexical_import_kind"] = kind; changed[i]["destination"] = destination
                changed[i]["resolution"] = resolution; changed[i]["resolution_basis"] = basis
                for key in ["target_file_id", "target_path", "target_revision", "target_sha256", "target_bytes"] { changed[i][key] = NSNull() }
            }
            other["links"] = changed
            let edge = try decode(other, index: index).links[0]
            precondition(!edge.canOpenTarget && edge.targetFile == nil && edge.evidenceLabel.contains(label))
            changed[0]["target_file_id"] = "4"; changed[0]["target_path"] = ProjectLinksFixture.path("src/detail/value.h")
            other["links"] = changed; refuse(other, index: index)
        }
        var partial = value
        partial["scanned_source_files"] = "1"; partial["unsupported_source_files"] = "1"
        partial["relationships_complete"] = false
        let page = try decode(partial, index: index)
        precondition(!page.complete && page.summary.contains("1 unsupported") && page.links[0].canOpenTarget)
        partial["relationships_complete"] = true; refuse(partial, index: index)
    }

    /// A proven inline ancestor targets this exact source capture, with no
    /// separate module-byte location or permission to substitute another file.
    private static func inlineRustUseWire(_ index: AtlasPreparedIndex) throws {
        var value = ProjectLinksFixture.importPage(owner: index.basis.owner, generation: 1, command: "imports-page")
        var rows = value["links"] as! [[String: Any]]
        for i in rows.indices {
            rows[i]["lexical_import_kind"] = "rust-use"; rows[i]["destination"] = "self::*"
            rows[i]["resolution_basis"] = "rust-use-inline-module-candidate"
            for (target, source) in [("target_file_id", "source_file_id"), ("target_path", "source_path"),
                ("target_revision", "source_revision"), ("target_sha256", "source_sha256"), ("target_bytes", "source_bytes")] {
                rows[i][target] = rows[i][source]
            }
        }
        value["links"] = rows
        let link = try decode(value, index: index).links[0]
        precondition(link.canOpenTarget && link.target == link.source && link.label.contains("inline module"))
        for (field, changed) in [
            ("target_file_id", "4" as Any), ("target_path", ProjectLinksFixture.path("src/other.rs")),
            ("target_revision", "2"), ("target_sha256", String(repeating: "f", count: 64)),
            ("target_bytes", "1"), ("lexical_import_kind", "rust-module")
        ] {
            var bad = value, changedRows = rows
            changedRows[0][field] = changed; bad["links"] = changedRows; refuse(bad, index: index)
        }
        for state in ["unavailable", "uncaptured", "outside-capture-scope"] {
            var bad = value, changedRows = rows
            changedRows[0]["resolution"] = state
            for field in ["target_revision", "target_sha256", "target_bytes"] { changedRows[0][field] = NSNull() }
            bad["links"] = changedRows; refuse(bad, index: index)
        }
        for side in [AtlasProjectLinkSide.evidence, .target] {
            var receipt = ProjectLinksFixture.importReceipt(owner: index.basis.owner, reader: 77,
                generation: 1, link: 1, side: .evidence)
            receipt["side"] = side.rawValue; receipt["relationship"] = rows[0]
            receipt["initial_original_offset"] = side == .evidence ? "11" : "0"
            let admitted = try AtlasProjectLinkReaderReceipt.decode(ProjectLinksFixture.json(receipt), index: index,
                generation: 1, link: link, side: side, destination: 77)
            precondition(admitted.info.identity.pathBytes == link.source.path.bytes)
            precondition(admitted.initialOriginalOffset == (side == .evidence ? 11 : 0))
            if side == .target { precondition(admitted.navigationDescription.contains("inline module")) }
        }
    }

    /// A use leaf may identify a conventional module file or only the module
    /// containing a possible item. Both remain lexical navigation candidates.
    private static func rustUseWire(_ index: AtlasPreparedIndex) throws {
        let original = ProjectLinksFixture.importPage(owner: index.basis.owner,
            generation: 1, command: "imports-page")
        let captureFields = ["target_revision", "target_sha256", "target_bytes"]
        let targetFields = ["target_file_id", "target_path"] + captureFields
        let localBases = ["rust-use-module-file-candidate", "rust-use-module-prefix-candidate"]
        for basis in localBases {
            var value = original, rows = original["links"] as! [[String: Any]]
            for i in rows.indices {
                rows[i]["lexical_import_kind"] = "rust-use"
                rows[i]["destination"] = basis == localBases[0] ? "crate::child" : "crate::child::Child"
                rows[i]["resolution_basis"] = basis
            }
            value["links"] = rows
            let page = try decode(value, index: index), link = page.links[0]
            precondition(link.canOpenTarget && link.target?.id == 4
                && link.importEvidence?.kind == "rust-use"
                && link.evidenceLabel.contains("not compiler resolved"))
            precondition(link.label.contains("containing-module candidate") == (basis == localBases[1]))

            for resolution in ["unavailable", "uncaptured", "outside-capture-scope"] {
                var unavailable = value, unavailableRows = rows
                for i in unavailableRows.indices {
                    unavailableRows[i]["resolution"] = resolution
                    for field in captureFields { unavailableRows[i][field] = NSNull() }
                }
                unavailable["links"] = unavailableRows
                let candidate = try decode(unavailable, index: index).links[0]
                precondition(!candidate.canOpenTarget && candidate.targetFile == 4 && candidate.target == nil)
                precondition(candidate.label.lowercased().contains("containing-module candidate") == (basis == localBases[1]))
            }
            for (language, kind) in [("rust", "rust-module"), ("python", "python-from"),
                ("javascript", "javascript-import"), ("typescript", "javascript-export"),
                ("jsx", "javascript-import"), ("tsx", "javascript-export")] {
                var bad = value, changed = rows
                changed[0]["language"] = language; changed[0]["lexical_import_kind"] = kind
                bad["links"] = changed; refuse(bad, index: index)
            }
            for resolution in ["missing", "ambiguous", "external", "unresolved"] {
                var bad = value, changed = rows
                for i in changed.indices {
                    changed[i]["resolution"] = resolution
                    for field in targetFields { changed[i][field] = NSNull() }
                }
                bad["links"] = changed; refuse(bad, index: index)
            }
            var old = value; old["resolver"] = "fcb-closed-catalog-imports-v2"
            refuse(old, index: index)
            for side in [AtlasProjectLinkSide.evidence, .target] {
                var receipt = ProjectLinksFixture.importReceipt(owner: index.basis.owner, reader: 77,
                    generation: 1, link: 1, side: side)
                receipt["relationship"] = rows[0]
                func admit(_ wire: [String: Any]) throws -> AtlasProjectLinkReaderReceipt {
                    try AtlasProjectLinkReaderReceipt.decode(ProjectLinksFixture.json(wire), index: index,
                        generation: 1, link: link, side: side, destination: 77)
                }
                let admitted = try admit(receipt)
                precondition(admitted.initialOriginalOffset == (side == .evidence ? 11 : 0))
                if side == .target {
                    precondition(admitted.navigationDescription.contains("containing-module candidate") == (basis == localBases[1]))
                    if basis == localBases[1] {
                        precondition(admitted.navigationDescription.contains("imported item location and compiler resolution are not established"))
                    }
                } else {
                    precondition(admitted.navigationDescription.contains("exact import evidence"))
                }
                var bad = receipt, changed = rows[0]
                changed["resolution_basis"] = basis == localBases[0] ? localBases[1] : localBases[0]
                bad["relationship"] = changed
                do { _ = try admit(bad); preconditionFailure("Changed Rust candidate provenance admitted") } catch {}
            }
        }

        var ambiguous = original, rows = original["links"] as! [[String: Any]]
        for i in rows.indices {
            rows[i]["lexical_import_kind"] = "rust-use"; rows[i]["destination"] = "crate::child"
            rows[i]["resolution"] = "ambiguous"; rows[i]["resolution_basis"] = "ambiguous-rust-crate-root"
            for field in targetFields { rows[i][field] = NSNull() }
        }
        ambiguous["links"] = rows
        let refusedRoot = try decode(ambiguous, index: index).links[0]
        precondition(!refusedRoot.canOpenTarget && refusedRoot.targetFile == nil && refusedRoot.targetPath == nil
            && refusedRoot.label.contains("crate roots") && refusedRoot.label.contains("no target was selected"))
        for (language, kind) in [("rust", "rust-module"), ("python", "python-from"), ("javascript", "javascript-import")] {
            var bad = ambiguous, changed = rows
            changed[0]["language"] = language; changed[0]["lexical_import_kind"] = kind
            bad["links"] = changed; refuse(bad, index: index)
        }
        for resolution in ["missing", "external", "unresolved"] {
            var bad = ambiguous, changed = rows
            changed[0]["resolution"] = resolution; bad["links"] = changed
            refuse(bad, index: index)
        }
        var forgedTarget = original, targetRows = original["links"] as! [[String: Any]]
        targetRows[0]["lexical_import_kind"] = "rust-use"
        targetRows[0]["resolution_basis"] = "ambiguous-rust-crate-root"
        forgedTarget["links"] = targetRows; refuse(forgedTarget, index: index)
    }


    private static func javascriptLoadWire(_ index: AtlasPreparedIndex) throws {
        let original = ProjectLinksFixture.importPage(owner: index.basis.owner,
            generation: 1, command: "imports-page")
        for (kind, label) in [
            ("javascript-require", "binding not resolved"),
            ("javascript-dynamic-import", "Literal import() expression")
        ] {
            var value = original, rows = original["links"] as! [[String: Any]]
            for i in rows.indices {
                rows[i]["language"] = "javascript"; rows[i]["lexical_import_kind"] = kind
                rows[i]["destination"] = "./child"
                rows[i]["resolution_basis"] = "unique-relative-module-candidate"
                rows[i]["source_path"] = ProjectLinksFixture.path("src/main.cjs")
                rows[i]["target_path"] = ProjectLinksFixture.path("src/child.cjs")
            }
            value["links"] = rows
            let page = try decode(value, index: index), link = page.links[0]
            precondition(link.canOpenTarget && link.importEvidence?.kind == kind)
            precondition(link.evidenceLabel.contains(label) && link.evidenceLabel.contains("not compiler resolved"))
            for (key, wrong) in [("language", "rust"), ("language", "python"),
                ("resolution_basis", "rust-module-file-candidate"),
                ("resolution_basis", "python-relative-module-candidate"),
                ("resolution_basis", "cpp-source-relative-include-candidate")] {
                var bad = value, changed = rows
                changed[0][key] = wrong; bad["links"] = changed; refuse(bad, index: index)
            }
            for (key, old) in [("extractor", "franken-markdown-d6fbd1d5/import-statements-v4"),
                ("resolver", "fcb-closed-catalog-imports-v4"),
                ("extraction_policy", "rust-use-tree-js-python-and-cpp-literal-include-v4")] {
                var bad = value; bad[key] = old; refuse(bad, index: index)
            }
            // A readable external literal is still not target-opening authority.
            for i in rows.indices {
                rows[i]["resolution"] = "external"; rows[i]["resolution_basis"] = "external-specifier"
                rows[i]["destination"] = "node:fs"
                for key in ["target_file_id", "target_path", "target_revision", "target_sha256", "target_bytes"] {
                    rows[i][key] = NSNull()
                }
            }
            value["links"] = rows
            let external = try decode(value, index: index).links[0]
            precondition(!external.canOpenTarget && external.importEvidence?.kind == kind)
        }
    }

    private static func typescriptSourceWire(_ index: AtlasPreparedIndex) throws {
        let original = ProjectLinksFixture.importPage(owner: index.basis.owner,
            generation: 1, command: "imports-page")
        for language in ["typescript", "tsx"] {
            for kind in ["javascript-import", "javascript-export", "javascript-require", "javascript-dynamic-import"] {
                var value = original, rows = original["links"] as! [[String: Any]]
                for i in rows.indices {
                    rows[i]["language"] = language; rows[i]["lexical_import_kind"] = kind
                    rows[i]["destination"] = "./child.js"
                    rows[i]["resolution_basis"] = "typescript-source-extension-candidate"
                    rows[i]["source_path"] = ProjectLinksFixture.path(language == "tsx" ? "src/main.tsx" : "src/main.ts")
                    rows[i]["target_path"] = ProjectLinksFixture.path("src/child.ts")
                }
                value["links"] = rows
                let admitted = try decode(value, index: index).links[0]
                precondition(admitted.canOpenTarget && admitted.importEvidence?.language == language
                    && admitted.importEvidence?.kind == kind
                    && admitted.target?.path.bytes == Array("src/child.ts".utf8)
                    && admitted.evidenceLabel.contains("not compiler resolved"))

                for resolution in ["unavailable", "uncaptured", "outside-capture-scope"] {
                    var unavailable = value, changed = rows
                    for i in changed.indices {
                        changed[i]["resolution"] = resolution
                        for key in ["target_revision", "target_sha256", "target_bytes"] { changed[i][key] = NSNull() }
                    }
                    unavailable["links"] = changed
                    let retained = try decode(unavailable, index: index).links[0]
                    precondition(!retained.canOpenTarget && retained.targetFile == 4
                        && retained.targetPath?.bytes == Array("src/child.ts".utf8)
                        && retained.importEvidence?.resolutionBasis == "typescript-source-extension-candidate",
                        "Typed catalog candidates remain visible without inventing an available source capture")
                }
                for (wrongLanguage, wrongKind) in [("javascript", kind), ("jsx", kind), ("rust", "rust-use")] {
                    var bad = value, changed = rows
                    changed[0]["language"] = wrongLanguage; changed[0]["lexical_import_kind"] = wrongKind
                    bad["links"] = changed; refuse(bad, index: index)
                }
                for wrongKind in ["rust-module", "python-from"] {
                    var bad = value, changed = rows
                    changed[0]["lexical_import_kind"] = wrongKind; bad["links"] = changed
                    refuse(bad, index: index)
                }
                for resolution in ["missing", "ambiguous", "external", "unresolved"] {
                    var bad = value, changed = rows
                    changed[0]["resolution"] = resolution
                    for key in ["target_file_id", "target_path", "target_revision", "target_sha256", "target_bytes"] {
                        changed[0][key] = NSNull()
                    }
                    bad["links"] = changed; refuse(bad, index: index)
                }
                var old = value; old["resolver"] = "fcb-closed-catalog-imports-v8"
                refuse(old, index: index)
            }
        }
    }

    private static func rustPathModuleWire(_ index: AtlasPreparedIndex) throws {
        var value = ProjectLinksFixture.importPage(owner: index.basis.owner,
            generation: 1, command: "imports-page")
        let declaration = "#[path = \"platform/custom.rs\"]\npub mod child;"
        let span = UInt64(declaration.utf8.count)
        var rows = value["links"] as! [[String: Any]]
        for i in rows.indices {
            rows[i]["language"] = "rust"; rows[i]["lexical_import_kind"] = "rust-path-module"
            rows[i]["destination"] = "platform/custom.rs"
            rows[i]["resolution_basis"] = "rust-path-attribute-candidate"
            rows[i]["source_path"] = ProjectLinksFixture.path("src/lib.rs")
            rows[i]["target_path"] = ProjectLinksFixture.path("src/platform/custom.rs")
            let start = UInt64(11) + UInt64(i) * span
            rows[i]["evidence_range"] = ["start": String(start), "end": String(start + span)]
        }
        value["links"] = rows
        let link = try decode(value, index: index).links[0]
        precondition(link.canOpenTarget && link.importEvidence?.kind == "rust-path-module"
            && link.importEvidence?.resolutionBasis == "rust-path-attribute-candidate"
            && link.destination == "platform/custom.rs"
            && link.target?.path.bytes == Array("src/platform/custom.rs".utf8)
            && link.evidenceRange.start == 11 && link.evidenceRange.end == 11 + span
            && link.evidenceLabel.contains("Rust path module declaration")
            && link.evidenceLabel.contains("not compiler resolved"),
            "Path attributes retain their exact declaration span and named catalog target")

        for resolution in ["unavailable", "uncaptured", "outside-capture-scope"] {
            var unavailable = value, changed = rows
            for i in changed.indices {
                changed[i]["resolution"] = resolution
                for key in ["target_revision", "target_sha256", "target_bytes"] { changed[i][key] = NSNull() }
            }
            unavailable["links"] = changed
            let retained = try decode(unavailable, index: index).links[0]
            precondition(!retained.canOpenTarget && retained.targetFile == 4
                && retained.targetPath?.bytes == Array("src/platform/custom.rs".utf8)
                && retained.importEvidence?.resolutionBasis == "rust-path-attribute-candidate")
        }
        for (kind, label) in [("rust-include", "Rust include! file candidate"),
            ("rust-include-str", "Rust include_str! text candidate"), ("rust-include-bytes", "Rust include_bytes! data candidate")] {
            var includes = value, includedRows = rows
            for i in includedRows.indices {
                includedRows[i]["lexical_import_kind"] = kind
                includedRows[i]["resolution_basis"] = "rust-include-file-candidate"
            }
            includes["links"] = includedRows
            let included = try decode(includes, index: index).links[0]
            precondition(included.canOpenTarget && included.evidenceLabel.contains(label))
            includedRows[0]["resolution_basis"] = "rust-path-attribute-candidate"
            includes["links"] = includedRows
            refuse(includes, index: index)
            includedRows[0]["lexical_import_kind"] = "rust-module"
            includedRows[0]["resolution_basis"] = "rust-include-file-candidate"
            includes["links"] = includedRows
            refuse(includes, index: index)
        }
        for (language, kind) in [("rust", "rust-module"), ("rust", "rust-use"),
            ("python", "python-from"), ("javascript", "javascript-import"), ("typescript", "rust-path-module")] {
            var bad = value, changed = rows
            changed[0]["language"] = language; changed[0]["lexical_import_kind"] = kind
            bad["links"] = changed; refuse(bad, index: index)
        }
        for basis in ["rust-module-file-candidate", "rust-use-module-file-candidate",
            "exact-relative-path", "unique-relative-module-candidate"] {
            var bad = value, changed = rows
            changed[0]["resolution_basis"] = basis; bad["links"] = changed
            refuse(bad, index: index)
        }
        for resolution in ["missing", "ambiguous", "external", "unresolved"] {
            var bad = value, changed = rows
            changed[0]["resolution"] = resolution
            for key in ["target_file_id", "target_path", "target_revision", "target_sha256", "target_bytes"] {
                changed[0][key] = NSNull()
            }
            bad["links"] = changed; refuse(bad, index: index)
        }
        for (key, old) in [("extractor", "franken-markdown-d6fbd1d5/import-statements-v7"),
            ("extraction_policy", "rust-use-tree-js-literal-loads-python-cpp-go-java-v7"),
            ("resolver", "fcb-closed-catalog-imports-v9")] {
            var bad = value; bad[key] = old; refuse(bad, index: index)
        }
    }

    @MainActor private static func largeSourceReaders() async throws {
        precondition(AtlasProjectImportEvidence.sourceLimit == 1_048_576)
        for encoding in ["utf8", "utf16le", "utf16be"] {
            let fixture = ProjectLinksFixture(includesImports: true, largeImportEncoding: encoding)
            let model = AtlasProjectLinksModel(root: "/project", projection: .imports, transport: fixture.transport)
            model.prepare(); await settled(model)
            let page = model.page!, link = page.links[0]
            let unit: UInt64 = encoding == "utf8" ? 1 : 2
            let first: UInt64 = 1_048_576 - 70 * 11 * unit
            precondition(link.source.capturedBytes == 1_048_576 && link.evidenceRange.start == first
                && link.evidenceRange.end == first + 10 * unit && link.canOpenTarget)
            let source = try await model.open(link, from: page, side: .evidence, readerTransport: fixture.readerTransport)
            let target = try await model.open(link, from: page, side: .target, readerTransport: fixture.readerTransport)
            let raw = ProjectLinksFixture.largeImportSource(encoding)
            let statementHex = ProjectLinksFixture.hex(raw[Int(first)..<Int(first + 10 * unit)])
            precondition(source.reader.page?.start == first
                && source.reader.page?.identity.capturedBytes == 1_048_576
                && source.reader.page?.identity.encoding == encoding
                && source.reader.page?.text.hasPrefix("mod child;\n") == true
                && source.reader.page?.originalHex.hasPrefix(statementHex) == true
                && !source.reader.canRefresh,
                "Large import evidence must use the retained original-byte reader")
            precondition(target.reader.page?.start == 0 && target.reader.page?.identity.encoding == "utf8"
                && target.reader.page?.text.hasPrefix("pub struct Child;") == true)
            model.close()
            precondition(source.reader.canPin && target.reader.canPin)
            let last = first + 69 * 11 * unit
            source.reader.offsetInput = String(last); source.reader.goToByte()
            let deadline = ProcessInfo.processInfo.systemUptime + 10
            while source.reader.busy && ProcessInfo.processInfo.systemUptime < deadline { await Task.yield() }
            precondition(!source.reader.busy && source.reader.page?.start == last
                && source.reader.page?.text == "mod child;\n"
                && source.reader.page?.originalHex == ProjectLinksFixture.hex(raw[Int(last)...]),
                "Closing the inventory must preserve independent reading and lossless copy bytes near EOF")
            precondition(fixture.index.count("create:") == 1
                && fixture.log.filter { $0 == "imports-begin" }.count == 1
                && fixture.log.filter { $0 == "imports-step" }.count == 2)
            source.reader.close(); target.reader.close()
        }
    }

    @MainActor static func main() async throws {
        try await largeSourceReaders()
        let fixture = ProjectLinksFixture(includesImports: true), index = try await prepared(fixture)
        try ProjectLinksGeometryTests.run(index, projection: .imports)
        try wire(index)
        try pythonWire(index)
        try cppWire(index)
        try goWire(index)
        try cargoDependencyWire(index)
        try cargoWorkspaceMetadataWire(index)
        try javaWire(index)
        try javascriptLoadWire(index)
        try typescriptSourceWire(index)
        try rustUseWire(index)
        try inlineRustUseWire(index)
        try rustPathModuleWire(index)
        let live = ProjectLinksFixture(includesImports: true)
        let model = AtlasProjectLinksModel(root: "/project", transport: live.transport)
        precondition(model.availableProjections == [.documentation, .imports])
        model.prepare(); await settled(model)
        let documentation = model.page!
        let documentReader = try await model.open(documentation.links[0], from: documentation,
            side: .evidence, readerTransport: live.readerTransport)
        model.selectProjection(.imports); await settled(model)
        let imports = model.page!
        precondition(imports.projection == .imports && imports.generation == 1 && live.index.count("create:") == 1,
            "Imports must reuse the frozen capture without replacing the Markdown inventory")
        let evidence = try await model.open(imports.links[0], from: imports, side: .evidence, readerTransport: live.readerTransport)
        let target = try await model.open(imports.links[0], from: imports, side: .target, readerTransport: live.readerTransport)
        precondition(evidence.reader.page?.start == 11 && evidence.reader.page?.text.hasPrefix("mod child;") == true
            && evidence.evidenceLabel.contains("11..<21") && !evidence.reader.canRefresh)
        precondition(target.reader.page?.start == 0 && target.reader.page?.text.hasPrefix("pub struct Child;") == true
            && target.evidenceLabel.contains("candidate") && target.evidenceLabel.contains("Compiler resolution"))
        let creates = live.log.filter { $0 == "create-reader" }.count
        do {
            _ = try await model.open(documentation.links[0], from: documentation, side: .evidence, readerTransport: live.readerTransport)
            preconditionFailure("Equal generation in another projection authorized an old row")
        } catch {}
        precondition(live.log.filter { $0 == "create-reader" }.count == creates)
        model.selectPath(Array("src/child.rs".utf8), direction: .incoming); await settled(model)
        precondition(model.page?.total == 70 && model.page?.filterFile == 4)
        model.loadPage(start: 64); await settled(model)
        precondition(model.page?.links.count == 6 && model.page?.links.first?.id == 65)
        model.selectProjection(.documentation); await settled(model)
        precondition(model.page?.projection == .documentation && model.page?.total == 0
            && model.page?.generation == 1 && model.selectedPath == Array("src/child.rs".utf8)
            && live.log.filter { $0 == "begin" }.count == 1,
            "Projection switching preserves the exact file filter and previously built inventory")
        model.selectProjection(.imports); await settled(model)
        precondition(model.page?.total == 70 && model.page?.generation == 1
            && live.log.filter { $0 == "imports-begin" }.count == 1)
        model.prepare(); await settled(model)
        precondition(model.page?.generation == 1 && live.index.count("create:") == 1
            && live.log.filter { $0 == "imports-begin" }.count == 1,
            "Reloading imports retains its accepted backend generation without recapture or extraction")
        model.selectProjection(.documentation); await settled(model)
        precondition(model.page?.generation == 1, "Reloading imports preserves the Markdown inventory")

        model.selectProjection(.imports); await settled(model)
        let accepted = model.page!
        live.patch { phase, value in
            var value = value
            if phase == "imports-reader" { value["initial_original_offset"] = "12" }
            return value
        }
        let closedBefore = live.log.filter { $0 == "close-reader" }.count
        do {
            _ = try await model.open(accepted.links[0], from: accepted, side: .evidence, readerTransport: live.readerTransport)
            preconditionFailure("Changed evidence offset published a reader")
        } catch {}
        precondition(live.log.filter { $0 == "close-reader" }.count == closedBefore + 1 && evidence.reader.page?.start == 11)
        live.patch { _, value in value }

        live.patch { phase, value in
            var value = value
            if phase == "imports-step", value["examined_source_files"] as? String == "2" {
                value["relationship_generation"] = "999"
            }
            return value
        }
        for _ in 0..<2 {
            model.prepare(refresh: true); await settled(model)
            precondition(model.page?.id == accepted.id && model.page?.basis == accepted.basis
                && model.projection == .imports && model.direction == .incoming
                && model.selectedPath == Array("src/child.rs".utf8)
                && model.canFollowDocumentLinks(from: documentReader),
                "Repeated rejected import replacements preserve both accepted projection inventories")
        }
        live.patch { _, value in value }
        live.pause("imports-step"); model.prepare(refresh: true)
        let refreshDeadline = ProcessInfo.processInfo.systemUptime + 10
        while !live.isBlocked && ProcessInfo.processInfo.systemUptime < refreshDeadline { await Task.yield() }
        precondition(live.isBlocked && model.page?.id == accepted.id
            && model.relationshipProgress?.projection == .imports,
            "Import refresh progress must not replace accepted rows")
        model.cancel(); live.gate.signal(); await settled(model)
        precondition(model.page?.id == accepted.id && model.projection == .imports
            && model.relationshipProgress == nil && model.canFollowDocumentLinks(from: documentReader))
        let retainedTarget = try await model.open(accepted.links[0], from: accepted, side: .target,
            readerTransport: live.readerTransport)
        precondition(retainedTarget.reader.page?.start == 0,
            "Failed and canceled import refreshes leave original catalog targets openable")
        retainedTarget.reader.close()
        let createsBeforeRefresh = live.index.count("create:")
        model.prepare(refresh: true); await settled(model)
        precondition(model.page?.generation == 1 && model.page?.basis != accepted.basis
            && live.index.count("create:") == createsBeforeRefresh + 1
            && !model.canFollowDocumentLinks(from: documentReader))
        model.selectProjection(.documentation); await settled(model)
        precondition(model.page?.generation == 1 && live.log.filter { $0 == "begin" }.count == 2,
            "A new capture invalidates both projection inventories")
        model.close()
        precondition(documentReader.reader.page != nil && evidence.reader.page?.start == 11 && target.reader.page?.start == 0,
            "Opened captures survive closing the relationship owner")
        documentReader.reader.close(); evidence.reader.close(); target.reader.close()

        let canceled = ProjectLinksFixture(includesImports: true)
        let canceledModel = AtlasProjectLinksModel(root: "/project", transport: canceled.transport)
        canceledModel.prepare(); await settled(canceledModel)
        let retainedDocumentation = canceledModel.page!
        canceled.pause("imports-step"); canceledModel.selectProjection(.imports)
        let deadline = ProcessInfo.processInfo.systemUptime + 10
        while !canceled.isBlocked && ProcessInfo.processInfo.systemUptime < deadline { await Task.yield() }
        precondition(canceled.isBlocked && canceledModel.page?.id == retainedDocumentation.id
            && canceledModel.projection == .documentation && canceledModel.relationshipProgress?.projection == .imports)
        canceledModel.cancel(); canceled.gate.signal(); await settled(canceledModel)
        precondition(canceledModel.page?.id == retainedDocumentation.id && canceledModel.hasPreparedSources
            && canceledModel.projection == .documentation && canceledModel.relationshipProgress == nil,
            "Canceling an unbuilt projection preserves the accepted projection and rows")
        canceledModel.selectProjection(.documentation); await settled(canceledModel)
        precondition(canceledModel.page?.generation == 1 && canceled.log.filter { $0 == "begin" }.count == 1)
        canceledModel.selectProjection(.imports); await settled(canceledModel)
        precondition(canceledModel.page?.generation == 2, "Canceled import attempts consume only their own namespace")
        canceledModel.close()

        let retiring = ProjectLinksFixture(includesImports: true)
        let retiringModel = AtlasProjectLinksModel(root: "/project", projection: .imports, transport: retiring.transport)
        retiring.pause("imports-begin"); retiringModel.prepare()
        let closeDeadline = ProcessInfo.processInfo.systemUptime + 10
        while !retiring.isBlocked && ProcessInfo.processInfo.systemUptime < closeDeadline { await Task.yield() }
        precondition(retiring.isBlocked)
        retiringModel.close(); retiring.gate.signal(); await settled(retiringModel)
        precondition(retiringModel.page == nil && !retiringModel.hasPreparedSources)
        print("AtlasProjectImportsTests: projection reuse, exact evidence, target candidates and cancellation passed")
    }
}
