import Foundation

extension AtlasReadingDeskModel {
    /// Keep the exact original range the user saw, including a retained search
    /// or supplied capture. Source replacement while the desk starts revokes
    /// this intent; it cannot silently import the replacement capture instead.
    func keep(reader: AtlasPagedReaderModel) {
        guard canOperate, reader.canPin, let original = reader.page,
              let expectedDomain = readerDomain, reader.retainedRegistryDomain == expectedDomain else { return }
        keepCapture(start: original.start, end: original.end, sourceDomain: expectedDomain) { work in
            guard let current = reader.page, current.identity.matches(original.identity), reader.canPin,
                  reader.retainedRegistryDomain == expectedDomain else {
                throw AtlasReadingDeskError.unavailable
            }
            return try await reader.withRetainedReader(work)
        }
    }
}
