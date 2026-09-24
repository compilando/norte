//! Listing/stat options ([`ListOptions`]) and the sanitized attribute
//! request ([`AttrRequest`]) that travels with them (#108 block 2, ADR 0039).

use norte_proto::ATTRS_MAX_REQUEST;

/// Requested attribute ids, sanitized: all valid per
/// [`norte_proto::is_valid_attr_id`], no duplicates (the first wins) and
/// at most [`ATTRS_MAX_REQUEST`]. This type FILTERS — rejecting a
/// malformed request with `-32602` is the daemon's job (and the CLI's,
/// before calling the embedded backend), *before* building one.
///
/// ```
/// use norte_vfs::AttrRequest;
/// let req = AttrRequest::sanitized(["posix.mode".to_owned(), "BAD".to_owned()]);
/// assert!(req.wants("posix.mode"));
/// assert!(!req.wants("BAD"));
/// assert!(!req.is_empty());
/// assert_eq!(req.iter().collect::<Vec<_>>(), ["posix.mode"]);
/// assert!(AttrRequest::default().is_empty());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttrRequest(Vec<String>);

impl AttrRequest {
    /// Builds by filtering: invalid id out, duplicate out (the first
    /// wins), truncated to [`ATTRS_MAX_REQUEST`].
    #[must_use]
    pub fn sanitized<I: IntoIterator<Item = String>>(ids: I) -> Self {
        let mut out: Vec<String> = Vec::new();
        for id in ids {
            if out.len() == ATTRS_MAX_REQUEST {
                break;
            }
            if norte_proto::is_valid_attr_id(&id) && !out.contains(&id) {
                out.push(id);
            }
        }
        Self(out)
    }

    /// No attributes requested: the provider must take its bare fast path.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Is `id` requested? Providers gate every materialization on this.
    #[must_use]
    pub fn wants(&self, id: &str) -> bool {
        self.0.iter().any(|have| have == id)
    }

    /// Requested ids, in request order.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }

    /// Emission belt (ADR 0039 §5), shared by the daemon and the embedded
    /// backend: keeps in `entry.attrs` only REQUESTED ids with a
    /// conforming value — `Text`/`Bytes` within the byte ceiling and never
    /// [`AttrValue::Unknown`](norte_proto::AttrValue::Unknown) (a
    /// conforming daemon never emits it, ADR 0039 §3). A buggy provider
    /// loses the cell, never breaks the page; nothing is silently
    /// truncated.
    ///
    /// ```
    /// use norte_proto::{AttrValue, Entry, EntryKind, VPath};
    /// use norte_vfs::AttrRequest;
    /// let req = AttrRequest::sanitized(["a.ok".to_owned()]);
    /// let mut e = Entry {
    ///     attrs: [
    ///         ("a.ok".to_owned(), AttrValue::Uint(1)),
    ///         ("a.nope".to_owned(), AttrValue::Uint(2)), // not requested
    ///         ("a.unk".to_owned(), AttrValue::Unknown),  // never emitted
    ///     ]
    ///     .into(),
    ///     path: VPath::parse("mem:///f").unwrap(),
    ///     kind: EntryKind::File,
    ///     size: None,
    ///     mtime_ms: None,
    /// };
    /// req.retain_conforming(&mut e);
    /// assert_eq!(e.attrs.len(), 1);
    /// assert!(e.attrs.contains_key("a.ok"));
    /// ```
    pub fn retain_conforming(&self, entry: &mut norte_proto::Entry) {
        entry.attrs.retain(|id, v| {
            self.wants(id)
                && match v {
                    norte_proto::AttrValue::Text(s) => s.len() <= norte_proto::ATTR_TEXT_MAX,
                    norte_proto::AttrValue::Bytes(b) => b.len() <= norte_proto::ATTR_BYTES_MAX,
                    norte_proto::AttrValue::Unknown => false,
                    _ => true,
                }
        });
    }
}

/// Options for [`Provider::list_with`](crate::Provider::list_with) and
/// [`Provider::stat_with`](crate::Provider::stat_with). A struct so future
/// listing options don't shake up every signature again.
///
/// ```
/// use norte_vfs::ListOptions;
/// assert!(ListOptions::default().attrs.is_empty());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListOptions {
    /// Attributes to materialize per entry. Empty = bare entries.
    pub attrs: AttrRequest,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitized_filters_invalid_dedups_first_wins_and_truncates() {
        let ids = vec![
            "posix.mode".to_owned(),
            "UPPER.no".to_owned(),   // invalid: uppercase
            "nodot".to_owned(),      // invalid: no dot
            "posix.mode".to_owned(), // duplicate
            "s3.etag".to_owned(),
        ];
        let req = AttrRequest::sanitized(ids);
        assert_eq!(req.iter().collect::<Vec<_>>(), ["posix.mode", "s3.etag"]);
        assert!(req.wants("posix.mode"));
        assert!(!req.wants("upper.no"));

        // Truncated to the wire ceiling: 20 valid ids → 16.
        let many = (0..20).map(|i| format!("a.b{i}"));
        assert_eq!(
            AttrRequest::sanitized(many).iter().count(),
            norte_proto::ATTRS_MAX_REQUEST
        );
    }

    #[test]
    fn default_is_empty_and_list_options_wraps_it() {
        assert!(AttrRequest::default().is_empty());
        assert!(ListOptions::default().attrs.is_empty());
    }
}
