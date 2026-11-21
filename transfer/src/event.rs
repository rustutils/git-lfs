/// Progress of one object in a
/// [`Transfer::download`](crate::Transfer::download) or
/// [`Transfer::upload`](crate::Transfer::upload).
///
/// Events for one object arrive in order: `Started`, any number of
/// `Progress`, then `Completed` or `Failed`. An object the batch response
/// marked with an error gets only `Failed`. Events of different objects
/// interleave. The [`Report`](crate::Report) is the authoritative outcome.
#[derive(Debug, Clone)]
pub enum Event {
    /// The object's transfer is about to start.
    ///
    /// `size` comes from the batch response, or from the request when the
    /// server sent 0.
    Started { oid: String, size: u64 },

    /// `bytes_done` bytes of the object have moved so far.
    ///
    /// A retried attempt starts counting again from zero, or from the
    /// resume offset of a resumed download, so the value can go down.
    Progress { oid: String, bytes_done: u64 },

    /// The transfer succeeded.
    ///
    /// A download is verified and in the store. An upload was accepted and
    /// verified if the server asked for it, or the server already had it.
    Completed { oid: String },

    /// The transfer failed, with the error's message.
    Failed { oid: String, error: String },
}
