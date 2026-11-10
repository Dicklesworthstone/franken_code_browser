import Foundation

/// Admitted policy and work receipt for one bounded expression query. The
/// engine owns parsing, path/language filters, predicates and source decoding.
/// Native code keeps the raw expression separate from its primary hit text.
struct AtlasSearchExpression: Equatable, Sendable {
    static let scanLimit: UInt64 = 256 * 1024 * 1024
    let query: String
    let primaryNeedle: String
    let maxScanBytes: UInt64
    let scannedInputBytes: UInt64
    let excludedFiles: Int
    let scopeFiles: Int

    /// Preserve the engine's specific bounded-query refusal without treating
    /// arbitrary foreign error text as a recognized query capability.
    static func queryFailure(in data: Data) -> AtlasSearchError? {
        struct Envelope: Decodable {
            struct Failure: Decodable { let code: String }
            let schema, status: String
            let error: Failure
        }
        guard let envelope = try? JSONDecoder().decode(Envelope.self, from: data),
              ["fcb.atlas-search/1", "fcb.atlas-session/1"].contains(envelope.schema), envelope.status == "error",
              ["QUERY_EMPTY", "QUERY_NEEDLE_TOO_LONG", "QUERY_LIMIT_EXCEEDED",
               "QUERY_REGEX_UNQUALIFIED", "QUERY_SYNTAX_ERROR"].contains(envelope.error.code) else { return nil }
        return .query(envelope.error.code)
    }

    static func == (a: Self, b: Self) -> Bool {
        a.samePolicy(as: b) && a.scannedInputBytes == b.scannedInputBytes
    }
    func advances(_ previous: Self) -> Bool {
        samePolicy(as: previous) && scannedInputBytes >= previous.scannedInputBytes
    }
    private func samePolicy(as other: Self) -> Bool {
        query.utf8.elementsEqual(other.query.utf8)
            && primaryNeedle.utf8.elementsEqual(other.primaryNeedle.utf8)
            && maxScanBytes == other.maxScanBytes
            && excludedFiles == other.excludedFiles && scopeFiles == other.scopeFiles
    }

    /// Flattened optional wire fields allow legacy literal/structural pages to
    /// remain compatible while rejecting expression metadata in another mode.
    /// All fields are required together when expression mode is requested.
    struct Wire: Decodable, Sendable {
        let expression, primary_needle, query_syntax, predicate_scope, path_filter_domain: String?
        let max_scan_bytes, scanned_input_bytes, expression_excluded_files, expression_scope_files: String?

        init(expression: String? = nil, primary_needle: String? = nil, query_syntax: String? = nil,
             predicate_scope: String? = nil, path_filter_domain: String? = nil,
             max_scan_bytes: String? = nil, scanned_input_bytes: String? = nil,
             expression_excluded_files: String? = nil, expression_scope_files: String? = nil) {
            self.expression = expression; self.primary_needle = primary_needle; self.query_syntax = query_syntax
            self.predicate_scope = predicate_scope; self.path_filter_domain = path_filter_domain
            self.max_scan_bytes = max_scan_bytes; self.scanned_input_bytes = scanned_input_bytes
            self.expression_excluded_files = expression_excluded_files; self.expression_scope_files = expression_scope_files
        }

        func admit(mode: AtlasSearchMode, query: String, needle: String, eligibleFiles: Int,
                   maximumScanBytes: UInt64 = AtlasSearchExpression.scanLimit) throws -> AtlasSearchExpression? {
            guard mode == .expression else {
                guard [expression, primary_needle, query_syntax, predicate_scope, path_filter_domain,
                       max_scan_bytes, scanned_input_bytes, expression_excluded_files, expression_scope_files]
                    .allSatisfy({ $0 == nil }) else { throw AtlasSearchError.invalidResponse }
                return nil
            }
            guard eligibleFiles >= 0, maximumScanBytes <= AtlasSearchExpression.scanLimit,
                  let expression, validText(expression), expression.utf8.elementsEqual(query.utf8),
                  let primary = primary_needle, validText(primary), primary.utf8.elementsEqual(needle.utf8),
                  query_syntax == "fcb-bounded-expression/1", predicate_scope == "same-complete-capture",
                  path_filter_domain == "utf8-or-escaped-search-key",
                  let limit = number(max_scan_bytes), limit == maximumScanBytes,
                  let scanned = number(scanned_input_bytes), scanned <= limit,
                  let excluded = number(expression_excluded_files), excluded <= UInt64(eligibleFiles),
                  let scope = number(expression_scope_files), scope == UInt64(eligibleFiles) - excluded else {
                throw AtlasSearchError.invalidResponse
            }
            return AtlasSearchExpression(query: expression, primaryNeedle: primary, maxScanBytes: limit,
                scannedInputBytes: scanned, excludedFiles: Int(excluded), scopeFiles: Int(scope))
        }
        private func validText(_ value: String) -> Bool {
            !value.isEmpty && value.utf8.count <= 1024 && !value.utf8.contains(0)
        }
        private func number(_ value: String?) -> UInt64? {
            guard let value, let number = UInt64(value), String(number) == value else { return nil }
            return number
        }
    }
}
