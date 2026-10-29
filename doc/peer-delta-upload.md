# Peer incremental uploads (2026-10-29)

The dev `twodrive-dev` WebDAV-over-QUIC transport now negotiates binary incremental
uploads. NAT traversal, relay routing, invitations and device authentication use
the existing iroh transport. This adds file-content deltas; WebDAV metadata still
uses full inventory polling. It does not change OneDrive/Graph uploads.

## Client and frontend contract

The same `WebDavBackend` is used by Linux FUSE and the Windows/Linux desktop file
service and upload-only backup workers. Existing `upload_controlled`,
`upload_file_with_version` and versioned byte uploads default to `UploadMode::Auto`.
Files below 128 KiB and new files use full PUT. An overwrite requires the current
quoted strong ETag. The receiver must advertise the exact v1 media type through
`OPTIONS` / `Accept-Patch` before any signature request is made.

Frontends provide a stable source snapshot, destination, observed ETag and a
cancellable progress callback. A source path must stay coherent during reading;
backup workers already spool their claimed generation. A filename/extension,
mtime or reported dirty range alone is never proof that other bytes are unchanged.
No editor-specific offsets or document parser is required. A future file manager
may provide more hints, but this implementation scans the complete local source
and verifies content independently. Scanning and disk reconstruction still read
whole files even when very little crosses the network.

Frontends can select `with_upload_mode(UploadMode::Full)` or call
`upload_controlled_with_mode`. The latter returns an `UploadOutcome` with metadata,
actual transfer kind, committed request-body bytes, reused bytes and downloaded
signature bytes. Counts exclude HTTP/QUIC framing, authentication and metadata
requests. Progress describes local work/transfer, not an independent proof of
remote completion; the operation response and refreshed metadata indicate success.
Hashing, encoding, validation and fallback remain in Rust backend modules. The
paired provider's schema supplies an advanced **始终整文件上传** checkbox, consumed
by the generic desktop UI on both operating systems, and by restored workers.

CLI examples (same flags on Linux and Windows):

```text
twodrive-dev --state PRIVATE_CLIENT webdav --url LOOPBACK_URL --auth-file PRIVATE_CREDENTIALS --write put LOCAL_SNAPSHOT /document.pdf --if-match '"CURRENT_ETAG"'
twodrive-dev --state PRIVATE_CLIENT webdav --full-upload --url LOOPBACK_URL --auth-file PRIVATE_CREDENTIALS --write put LOCAL_SNAPSHOT /document.pdf --if-match '"CURRENT_ETAG"'
```

`put` reports `upload=delta|full bytes=… reused=… signature=…`. For an isolated Linux
mount, `connect --mount MOUNT --write --full-upload` forces full uploads; omit the
flag for automatic negotiation. Generic external WebDAV clients that send only
PUT continue to send complete files. This does not teach Explorer or other
third-party clients the extension automatically.

## Wire protocol and safety

1. Authenticated `TD-SIGNATURE PATH` carries one exact `If-Match`. The receiver
   opens only a regular file inside the existing directory capability and returns
   a bounded v1 JSON signature: size, whole-file SHA-256, chunking parameters and
   ordered chunk lengths/SHA-256 values. URI paths use the same decoding and
   normalization as DAV. HTTP ETags keep their existing quoted format.
2. Both sides use deterministic gear content-defined chunks. The minimum is
   normally 8 KiB, the expected cut is around 16 KiB, and the maximum is 32 KiB.
   Minimum size grows with the base file to bound the table to 32,768 entries;
   the largest minimum is 1 MiB, largest chunk 4 MiB and largest base 32 GiB.
   Signatures are capped at 8 MiB. Inserts/deletes can resynchronize subsequent
   chunks; duplicate chunks may reuse any identical base occurrence.
3. The sender writes a patch to a private temporary file. It selects it only if
   patch body plus signature download is smaller than a complete upload.
   Otherwise it rewinds the source and uses conditional full PUT.
4. `PATCH PATH`, `Content-Type: application/vnd.twodrive.delta-v1`, and the same
   exact `If-Match` carry `TDDELTA1`, base SHA-256, target big-endian u64 length,
   target SHA-256, and operations. Tag 1 copies a big-endian u64 offset/u32 length;
   tag 2 carries a u32 length and literal bytes; tag 0 terminates. Zero lengths,
   unknown tags, invalid ranges, truncated/trailing data, wrong hashes and an
   oversized target are rejected. Output must match the declared size and hash.
5. The receiver spools the patch, reconstructs a separate temporary file, then
   invokes the existing DAV PUT handler with the original conditional/lock
   headers. The existing network mutation mutex covers this operation. DAV
   rechecks conditions and locks before its synced atomic replacement. Read-only
   and revoked peers cannot write. Failed/interrupted reconstruction never
   invokes PUT. Anonymous/private temporary files are removed on normal exits;
   disk space may temporarily include patch, reconstructed output and DAV's
   atomic-write copy. A process crash may leave DAV's existing hidden temp file;
   this work does not claim new crash-resume support.

Unsupported/older peers, unsupported v1 signatures, new/small files and an
unprofitable delta fall back to full PUT. Explicit unsupported or invalid-patch
replies (405/415/501/422) can fall back with the **same ETag**. A stale/missing
version (412), wrong base digest (409), lock, authorization, transport failure or
ambiguous server error does not silently retry a mutation. Retained local pending
work remains available for the normal recovery/reconciliation flow.

An already streaming oversized body can be stopped before its HTTP rejection is received; the gateway may report 502 instead of 413, or the active loopback HTTP writer may receive a broken-pipe/reset error before response headers. Both paths reject the mutation, and a 502 must not trigger an automatic full replay. Header-only limit rejection is tested separately. See [TD-20261029-02](incidents.md#td-20261029-02超限补丁回归把流式早拒绝固定断言为-http-413).

The upload limit bounds reconstructed content as well as ordinary PUT. Patch
input is capped at `max_upload + 1 MiB`. Hashing and chunk matching use bounded
memory; disk/CPU costs and the table cap are intentional tradeoffs. Local external
writers do not participate in the DAV mutation mutex. Content/version checks
reduce races, but do not establish a cross-process filesystem transaction against
an uncooperative local writer.

## Formats and verification boundaries

The codec operates on bytes, including PDF, DOC/DOCX, PPT/PPTX and other binary
files. DOCX/PPTX are compressed containers: preserved compressed entries can be
reused, but an editor that rewrites/recompresses most content may offer little
savings and trigger full upload. No promise is made about a fixed savings ratio
or semantic document merging.

`tests/delta_upload.rs` uses a real encrypted local QUIC connection and temporary
states. It checks insertion/deletion/modification reuse, stale versions, explicit
full upload, full fallback for entirely changed content/new files, corruption,
copy/output bounds, truncation, DAV locks, cancellation and temp cleanup.
`tests/integration.rs` also checks full fallback to a legacy DAV server and retains
its original content/type/capability/permission regressions. Codec tests check
roundtrip hashes, identical/empty targets and shifted content.

The existing two-process smoke now constructs a synthetic PDF and DEFLATE DOCX/
PPTX OPC containers with a common generated PNG, edits text/container offsets,
compares every transferred byte, verifies a smaller automatic delta, then forces
a full upload for each format. These are synthetic fixtures, not saves made by
Microsoft Office, LibreOffice or a PDF editor. Both OS jobs run the same smoke.
Neither runner represents two geographically separated NAT devices. Real editor
save patterns, Windows/Linux physical machines, arbitrary public networks,
production accounts and power-loss behavior remain separate acceptance scopes.

Related development fault and before/after evidence:
[TD-20261029-01](incidents.md#td-20261029-01增量扩展把-unicode-url-当作未解码磁盘路径).
