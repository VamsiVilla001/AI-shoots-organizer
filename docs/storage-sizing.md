# Scan storage and retention

Scan metadata, face vectors, detections, sampled-frame timestamps, identity
assignments and groups must persist until the user explicitly deletes the data.
Storage optimisation must preserve recognition values and user decisions.

Current formats: SQLite (`media.db`); little-endian float32 binary face vectors
and landmarks; JPEG thumbnails/crops; MP4 video proxies; shared ONNX AI models.
The existing portable catalogue archive already uses lossless ZIP Deflate.

The progress panel's **Space used (est.)** is a per-shoot logical record estimate
plus the file sizes of its saved previews. It refreshes every 30 seconds on a
background thread, independently of progress events. Record sizing includes all
columns of shoots, media, faces, clusters, albums, manual groups, jobs, video
detections, sampled frames and group/album links. Numbers are estimated at eight
bytes; text uses UTF-8 byte length and binary values use their original length.
File paths are deduplicated within the shoot; absent previews contribute zero.
Other filesystem errors produce an unavailable state rather than a false total.

This is not cloud billing or physical SQLite allocation. Shared player records,
models, indexes, row/page overhead, unused pages, temporary WAL files, exports,
logs and collaboration history are excluded. Assets reused by multiple shoots
can appear in each shoot's figure, so these figures should not be summed to
calculate organisation storage. The expanded UI explains the scope.

For cloud development, measure retained analysis separately from originals,
derived previews, indexes and backup retention. A 512-element float32 vector is
2,048 bytes: 100,000 such face vectors require about 195 MiB before other records
and indexes. Video frame count and faces per frame matter more than file count.

Lossless directions to benchmark before migration:

- Keep the exact vectors and records; evaluate lossless block compression with
  byte-for-byte round-trip tests and query-latency measurements. Floating-point
  vectors may compress only modestly.
- Reuse identical preview assets, with references preventing premature deletion.
- Generate full video proxies on demand independently of scan/AI completion.
- Checkpoint/reuse SQLite write logs safely and compact free pages during
  scheduled maintenance. Never delete an active WAL file manually.
- Share model assets across shoots and tenants where deployment permits.

No vector precision reduction, fewer analysis samples, automatic scan-result
expiry or data migration is introduced by the storage indicator change.
