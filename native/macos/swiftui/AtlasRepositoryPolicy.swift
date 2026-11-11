import Foundation

/// Shared receipt validation for native browsing and filename search. Rule-file
/// reads are configuration observations, separate from source-payload reads.
/// Legacy responses remain readable, but a repository-rules response must carry
/// its bounded evidence; a policy label alone never establishes completeness.
struct AtlasRepositoryPolicy: Equatable, Sendable {
    let name: String
    let evidence: AtlasRepositoryRuleEvidence?

    static func admit(name: String, enabled: Bool?, evidence: AtlasRepositoryRuleEvidence?,
        discoveryComplete: Bool) throws -> Self {
        switch name {
        case "product-defaults/no-rule-files-v1":
            guard enabled != true, evidence == nil else { throw AtlasRepositoryPolicyError.invalidResponse }
        case "repository-rules-v1":
            guard enabled == true, let evidence,
                  evidence.scopeComplete == discoveryComplete,
                  !discoveryComplete || evidence.observedRulesComplete else {
                throw AtlasRepositoryPolicyError.invalidResponse
            }
        default: throw AtlasRepositoryPolicyError.invalidResponse
        }
        return Self(name: name, evidence: evidence)
    }

    var summary: String {
        guard let evidence else { return "Default exclusions apply; repository ignore files were not read." }
        var text = "Repository ignore rules: \(evidence.filesLoaded) files applied."
        if !evidence.observedRulesComplete {
            text += " Some ignore rules could not be applied; affected paths were omitted."
            if let diagnostic = evidence.diagnostics.first {
                text += " \(diagnostic.path.display): \(diagnostic.message)."
            }
        }
        return text
    }
}

private enum AtlasRepositoryPolicyError: Error { case invalidResponse }

struct AtlasRepositoryRuleEvidence: Decodable, Equatable, Sendable {
    let observedRulesComplete, scopeComplete: Bool
    let configurationBytesRead, configurationReadCalls, configurationChecks: UInt64
    let filesRead, filesLoaded, rulesLoaded, compiledAdmissionBytes, matchSteps: UInt64
    let failedFiles, unresolvedPaths, diagnosticsOmitted: UInt64
    let diagnostics: [Diagnostic]

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Key.self)
        func number(_ key: Key) throws -> UInt64 {
            guard let value = AtlasReaderWire.integer(try c.decode(String.self, forKey: key)) else {
                throw AtlasRepositoryPolicyError.invalidResponse
            }
            return value
        }
        observedRulesComplete = try c.decode(Bool.self, forKey: .observed_rules_complete)
        scopeComplete = try c.decode(Bool.self, forKey: .scope_complete)
        configurationBytesRead = try number(.configuration_bytes_read)
        configurationReadCalls = try number(.configuration_read_calls)
        configurationChecks = try number(.configuration_checks)
        filesRead = try number(.files_read); filesLoaded = try number(.files_loaded)
        rulesLoaded = try number(.rules_loaded); compiledAdmissionBytes = try number(.compiled_admission_bytes)
        matchSteps = try number(.match_steps); failedFiles = try number(.failed_files)
        unresolvedPaths = try number(.unresolved_paths); diagnosticsOmitted = try number(.diagnostics_omitted)
        let limits = try c.nestedContainer(keyedBy: Limit.self, forKey: .limits)
        let expectedLimits: [(Limit, UInt64)] = [
            (.max_file_bytes, 16_384), (.max_total_bytes, 65_536), (.max_files, 256),
            (.max_checks, 32_768), (.max_read_calls, 65_536), (.max_rules, 1024),
            (.max_pattern_bytes, 512), (.max_compiled_bytes, 16_777_216),
            (.max_match_steps, 1_000_000), (.max_total_match_steps, 64_000_000)
        ]
        for (key, expected) in expectedLimits {
            guard AtlasReaderWire.integer(try limits.decode(String.self, forKey: key)) == expected else {
                throw AtlasRepositoryPolicyError.invalidResponse
            }
        }
        var items = try c.nestedUnkeyedContainer(forKey: .diagnostics)
        guard items.count.map({ $0 <= 32 }) ?? true else { throw AtlasRepositoryPolicyError.invalidResponse }
        var values: [Diagnostic] = []; values.reserveCapacity(items.count ?? 0)
        while !items.isAtEnd {
            guard values.count < 32 else { throw AtlasRepositoryPolicyError.invalidResponse }
            values.append(try items.decode(Diagnostic.self))
        }
        diagnostics = values
        guard configurationBytesRead <= 65_536, configurationReadCalls <= 65_536,
              configurationChecks <= 32_768, filesRead <= configurationChecks,
              filesLoaded <= filesRead, filesLoaded <= 256, rulesLoaded <= 1024,
              compiledAdmissionBytes <= 16_777_216, matchSteps <= 64_000_000,
              observedRulesComplete == (failedFiles == 0 && unresolvedPaths == 0),
              !scopeComplete || observedRulesComplete,
              !observedRulesComplete || (diagnostics.isEmpty && diagnosticsOmitted == 0) else {
            throw AtlasRepositoryPolicyError.invalidResponse
        }
    }

    struct Diagnostic: Decodable, Equatable, Sendable {
        let path: Path
        let code: String
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: Key.self)
            path = try c.decode(Path.self, forKey: .path)
            code = try c.decode(String.self, forKey: .code)
            guard code.hasPrefix("RULE_") || code.hasPrefix("SOURCE_"), code.utf8.count <= 64,
                  code.utf8.allSatisfy({ (65...90).contains($0) || $0 == 95 }) else {
                throw AtlasRepositoryPolicyError.invalidResponse
            }
        }
        var message: String {
            switch code {
            case "RULE_UNSUPPORTED_PATTERN": return "unsupported ignore pattern"
            case "RULE_INVALID_ENCODING": return "ignore file is not UTF-8"
            case "RULE_FILE_CHANGED": return "ignore file changed during discovery"
            case "RULE_SPECIAL_OBJECT": return "ignore path is not an admitted regular file"
            case "RULE_READ_FAILED", "RULE_SOURCE_UNAVAILABLE": return "ignore file could not be read"
            case "RULE_CHECK_LIMIT", "RULE_FILE_LIMIT", "RULE_BYTE_LIMIT", "RULE_READ_CALL_LIMIT",
                 "RULE_PATTERN_LIMIT", "RULE_COMPILE_LIMIT", "RULE_MATCH_WORK_LIMIT", "RULE_PATH_LIMIT":
                return "ignore policy exceeded its bounded work or storage allowance"
            default: return "ignore policy could not be verified (\(code))"
            }
        }
        private enum Key: CodingKey { case path, code }
    }
    struct Path: Decodable, Equatable, Sendable {
        let hex, display: String
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: Key.self)
            hex = try c.decode(String.self, forKey: .hex)
            display = try c.decode(String.self, forKey: .display)
            guard try c.decode(String.self, forKey: .encoding) == "unix-bytes",
                  hex.utf8.count <= 8192, display.utf8.count <= 32_768 else {
                throw AtlasRepositoryPolicyError.invalidResponse
            }
            let raw = try AtlasReaderWire.pathBytes(hex)
            guard raw.first != 47, !raw.contains(0), raw.isEmpty || !raw.split(separator: 47,
                omittingEmptySubsequences: false).contains(where: {
                    $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
                }) else { throw AtlasRepositoryPolicyError.invalidResponse }
        }
        private enum Key: CodingKey { case encoding, hex, display }
    }
    private enum Key: CodingKey {
        case observed_rules_complete, scope_complete, configuration_bytes_read, configuration_read_calls,
             configuration_checks, files_read, files_loaded, rules_loaded, compiled_admission_bytes,
             match_steps, failed_files, unresolved_paths, diagnostics_omitted, limits, diagnostics
    }
    private enum Limit: CodingKey {
        case max_file_bytes, max_total_bytes, max_files, max_checks, max_read_calls, max_rules,
             max_pattern_bytes, max_compiled_bytes, max_match_steps, max_total_match_steps
    }
}
