/// Per-object lifecycle events emitted by [`Transfer::download`] and
/// [`Transfer::upload`](crate::Transfer::upload).
///
/// Sent on the optional [`tokio::sync::mpsc::UnboundedSender`] passed in
/// by the caller. Order across objects is unspecified — events for one
/// object are ordered (Started → Progress* → Completed | Failed).
///
/// `Failed` carries a stringified error so the typed
/// [`TransferError`](crate::TransferError) can still be moved into
/// [`Report`](crate::Report) — events are for display, the report is
/// authoritative.
#[derive(Debug, Clone)]
pub enum Event {
    /// Transfer for `oid` is about to start. `size` is the byte count the
    /// server reported (or the local size, for uploads).
    Started { oid: String, size: u64 },

    /// `bytes_done` cumulative bytes have moved for this object so far.
    /// May fire many times per object; consumers should treat values as
    /// monotonically non-decreasing.
    Progress { oid: String, bytes_done: u64 },

    /// Transfer succeeded — for downloads, bytes are in the store and
    /// hash-verified; for uploads, the server's verify callback (if any)
    /// has returned 2xx.
    Completed { oid: String },

    /// Transfer failed after exhausting retries.
    Failed { oid: String, error: String },
}
